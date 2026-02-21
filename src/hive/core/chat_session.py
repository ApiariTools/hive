"""Chat session loop using claude-agent-sdk for multi-turn conversations.

Supports two modes:
  - Interactive: terminal-based chat (run_chat_loop)
  - Coordinator daemon: file-based IPC loop (run_coordinator_loop)
"""

from __future__ import annotations

import asyncio
import json
import os
import sys
import threading
from datetime import datetime, timezone
from pathlib import Path
from uuid import uuid4

from rich.console import Console
from rich.markdown import Markdown
from rich.text import Text

SESSION_FILE = "chat_session.json"

_console = Console(highlight=False)

_SPINNER_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


class _Spinner:
    """Animated spinner on stdout. Cleared when stop() is called."""

    def __init__(self, message: str = "thinking", *, daemon_mode: bool = False):
        self._message = message
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self._active = False
        self._daemon_mode = daemon_mode

    def start(self) -> None:
        if self._daemon_mode:
            return
        self._stop.clear()
        self._active = True
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self) -> None:
        if self._daemon_mode or not self._active:
            return
        self._active = False
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=1)
        sys.stdout.write("\r\033[2K")
        sys.stdout.flush()

    def _run(self) -> None:
        i = 0
        while not self._stop.wait(0.08):
            frame = _SPINNER_FRAMES[i % len(_SPINNER_FRAMES)]
            sys.stdout.write(f"\r\033[36m{frame} {self._message}\033[0m")
            sys.stdout.flush()
            i += 1


def _load_session_id(hive_dir: Path) -> str | None:
    """Load saved session ID from .hive/chat_session.json."""
    path = hive_dir / SESSION_FILE
    if not path.exists():
        return None
    try:
        with open(path) as f:
            data = json.load(f)
        return data.get("session_id")
    except (json.JSONDecodeError, OSError):
        return None


def _save_session_id(hive_dir: Path, session_id: str) -> None:
    """Save session ID to .hive/chat_session.json."""
    hive_dir.mkdir(parents=True, exist_ok=True)
    path = hive_dir / SESSION_FILE
    with open(path, "w") as f:
        json.dump({"session_id": session_id}, f)


# ---------------------------------------------------------------------------
# Coordinator file-based IPC helpers
# ---------------------------------------------------------------------------


def _coordinator_dir(hive_dir: Path) -> Path:
    return hive_dir / "coordinator"


def _read_coordinator_inbox(cdir: Path) -> list[dict]:
    """Read and clear coordinator inbox messages atomically."""
    path = cdir / "inbox.jsonl"
    if not path.exists():
        return []
    try:
        with open(path) as f:
            lines = f.readlines()
        if not lines:
            return []
        path.unlink()
        messages = []
        for line in lines:
            line = line.strip()
            if line:
                try:
                    messages.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
        return messages
    except OSError:
        return []


def send_to_coordinator(hive_dir: Path, message: str, source: str = "terminal") -> None:
    """Send a message to the coordinator's inbox."""
    cdir = _coordinator_dir(hive_dir)
    cdir.mkdir(parents=True, exist_ok=True)
    path = cdir / "inbox.jsonl"
    entry = {
        "id": str(uuid4()),
        "message": message,
        "source": source,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    with open(path, "a") as f:
        f.write(json.dumps(entry) + "\n")


def _append_coordinator_history(cdir: Path, role: str, content: str) -> None:
    """Append a message to coordinator history.jsonl."""
    cdir.mkdir(parents=True, exist_ok=True)
    path = cdir / "history.jsonl"
    entry = {
        "role": role,
        "content": content,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    with open(path, "a") as f:
        f.write(json.dumps(entry) + "\n")


def read_coordinator_history(hive_dir: Path, lines: int = 50) -> list[dict]:
    """Read recent coordinator history entries."""
    cdir = _coordinator_dir(hive_dir)
    path = cdir / "history.jsonl"
    if not path.exists():
        return []
    try:
        with open(path) as f:
            all_lines = f.readlines()
        entries = []
        for raw in all_lines[-lines:]:
            raw = raw.strip()
            if raw:
                try:
                    entries.append(json.loads(raw))
                except json.JSONDecodeError:
                    pass
        return entries
    except OSError:
        return []


# ---------------------------------------------------------------------------
# Interactive chat loop (terminal-based, existing behavior)
# ---------------------------------------------------------------------------


async def run_chat_loop(
    system_prompt: str,
    cwd: Path,
    hive_dir: Path,
    resume_session: bool = True,
    model: str | None = None,
) -> None:
    """Run the interactive chat loop."""
    from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient

    # Allow launching Claude Code from within an existing session (e.g. hive up)
    os.environ.pop("CLAUDECODE", None)

    session_id = _load_session_id(hive_dir) if resume_session else None

    options = ClaudeAgentOptions(
        system_prompt=system_prompt,
        cwd=str(cwd),
        permission_mode="bypassPermissions",
        model=model,
    )

    if session_id:
        options.resume = session_id
        _console.print(f"[dim]Resuming session {session_id[:8]}...[/dim]")
    else:
        _console.print("[dim]Starting new chat session...[/dim]")

    _console.print("[dim]Type /new for a fresh session, /quit to exit.[/dim]\n")

    async with ClaudeSDKClient(options=options) as client:
        if not session_id:
            spinner = _Spinner("connecting")
            spinner.start()
            await client.query("Hello. Briefly introduce yourself in one sentence.")
            session_id = await _receive_and_display(client, spinner)
            if session_id:
                _save_session_id(hive_dir, session_id)

        while True:
            try:
                user_input = await asyncio.to_thread(_read_input)
            except (EOFError, KeyboardInterrupt):
                _console.print("\n[dim]Exiting chat.[/dim]")
                break

            if user_input is None:
                _console.print("\n[dim]Exiting chat.[/dim]")
                break

            stripped = user_input.strip()
            if stripped == "/quit":
                _console.print("[dim]Exiting chat.[/dim]")
                break
            elif stripped == "/new":
                _console.print("[dim]Starting fresh session...[/dim]\n")
                return "restart"
            elif stripped == "/status":
                sid = session_id or "(new)"
                _console.print(f"[dim]Session: {sid}[/dim]")
                _console.print(f"[dim]Working dir: {cwd}[/dim]")
                continue
            elif not stripped:
                continue

            spinner = _Spinner("thinking")
            spinner.start()

            await client.query(stripped)
            new_sid = await _receive_and_display(client, spinner)
            if new_sid:
                session_id = new_sid
                _save_session_id(hive_dir, session_id)


# ---------------------------------------------------------------------------
# Coordinator daemon loop (file-based IPC, no terminal input)
# ---------------------------------------------------------------------------


async def run_coordinator_loop(
    system_prompt: str,
    cwd: Path,
    hive_dir: Path,
    model: str | None = None,
    poll_interval: float = 2.0,
    console: Console | None = None,
) -> None:
    """Non-interactive coordinator loop. Reads from inbox, writes to history.

    Runs as a daemon process. Clients (terminal, Telegram, Discord) write to
    .hive/coordinator/inbox.jsonl and read from .hive/coordinator/history.jsonl.
    """
    from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient

    from hive.core.daemon import StatusHeartbeat, install_signal_handlers, write_status

    os.environ.pop("CLAUDECODE", None)

    con = console or _console
    cdir = _coordinator_dir(hive_dir)
    cdir.mkdir(parents=True, exist_ok=True)

    # Set up status heartbeat
    status_path = cdir / "status.json"
    heartbeat = StatusHeartbeat(status_path)

    def _on_shutdown():
        write_status(status_path, "stopped", pid=os.getpid())

    install_signal_handlers(cleanup=_on_shutdown)
    heartbeat.set_state("starting", pid=os.getpid())

    # Load or create session
    session_path = cdir / "session.json"
    session_id = None
    if session_path.exists():
        try:
            with open(session_path) as f:
                data = json.load(f)
            session_id = data.get("session_id")
        except (json.JSONDecodeError, OSError):
            pass

    options = ClaudeAgentOptions(
        system_prompt=system_prompt,
        cwd=str(cwd),
        permission_mode="bypassPermissions",
        model=model,
    )

    if session_id:
        options.resume = session_id
        con.print(f"[dim]Coordinator: resuming {session_id[:8]}...[/dim]")
    else:
        con.print("[dim]Coordinator: starting new session...[/dim]")

    async with ClaudeSDKClient(options=options) as client:
        # Initial greeting if new session
        if not session_id:
            con.print("[dim]Sending initial greeting...[/dim]")
            await client.query("Hello. Briefly introduce yourself in one sentence.")
            new_sid, response = await _receive_and_log_coordinator(
                client, cdir, console=con,
            )
            if new_sid:
                session_id = new_sid
                _save_coordinator_session(cdir, session_id)

        heartbeat.set_state("idle", pid=os.getpid())
        con.print("[dim]Coordinator: listening for messages...[/dim]")

        while True:
            heartbeat.tick()
            messages = _read_coordinator_inbox(cdir)
            if messages:
                for msg in messages:
                    text = msg.get("message", "")
                    source = msg.get("source", "unknown")
                    if not text:
                        continue

                    con.print()
                    con.print(f"[yellow]━━━ message from {source} ━━━[/yellow]")
                    con.print(f"[yellow]{text}[/yellow]\n")

                    _append_coordinator_history(cdir, "user", text)

                    heartbeat.set_state("running", pid=os.getpid())
                    await client.query(text)
                    new_sid, response = await _receive_and_log_coordinator(
                        client, cdir, console=con,
                    )
                    if new_sid:
                        session_id = new_sid
                        _save_coordinator_session(cdir, session_id)
                    heartbeat.set_state("idle", pid=os.getpid())
            else:
                await asyncio.sleep(poll_interval)


def _save_coordinator_session(cdir: Path, session_id: str) -> None:
    """Save coordinator session ID."""
    path = cdir / "session.json"
    with open(path, "w") as f:
        json.dump({
            "session_id": session_id,
            "updated_at": datetime.now(timezone.utc).isoformat(),
        }, f)


async def _receive_and_log_coordinator(
    client,
    cdir: Path,
    console: Console | None = None,
) -> tuple[str | None, str]:
    """Receive response, log to history, return (session_id, full_response)."""
    from claude_agent_sdk import AssistantMessage, ResultMessage, SystemMessage
    from claude_agent_sdk.types import TextBlock, ToolUseBlock

    con = console or _console
    session_id = None
    response_parts: list[str] = []

    try:
        async for message in _safe_receive(client):
            if isinstance(message, AssistantMessage):
                for block in message.content:
                    if isinstance(block, TextBlock):
                        con.print()
                        con.print("[cyan]\u25cf[/cyan]", end=" ")
                        con.print(Markdown(block.text.strip()))
                        response_parts.append(block.text.strip())
                    elif isinstance(block, ToolUseBlock):
                        _display_tool_use(block, console=con)
            elif isinstance(message, ResultMessage):
                if message.is_error and message.result:
                    con.print(f"[bold red]error:[/bold red] {message.result}")
                    response_parts.append(f"ERROR: {message.result}")
                session_id = message.session_id
            elif isinstance(message, SystemMessage):
                pass
    except Exception as e:
        con.print(f"[bold red]error:[/bold red] {e}")

    full_response = "\n\n".join(response_parts)
    if response_parts:
        _append_coordinator_history(cdir, "assistant", full_response)

    return session_id, full_response


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------


async def _receive_and_display(client, spinner: _Spinner) -> str | None:
    """Receive all messages for one response, display them, return session_id."""
    from claude_agent_sdk import AssistantMessage, ResultMessage, SystemMessage
    from claude_agent_sdk._internal.message_parser import MessageParseError
    from claude_agent_sdk.types import TextBlock, ToolUseBlock

    session_id = None

    def _stop_spinner():
        spinner.stop()  # idempotent — safe to call multiple times

    try:
        async for message in _safe_receive(client):
            if isinstance(message, AssistantMessage):
                for block in message.content:
                    if isinstance(block, TextBlock):
                        _stop_spinner()
                        _console.print()
                        _console.print("[cyan]\u25cf[/cyan]", end=" ")
                        _console.print(Markdown(block.text.strip()))
                    elif isinstance(block, ToolUseBlock):
                        _stop_spinner()
                        _display_tool_use(block)
            elif isinstance(message, ResultMessage):
                _stop_spinner()
                if message.is_error and message.result:
                    _console.print(f"[bold red]error:[/bold red] {message.result}")
                session_id = message.session_id
            elif isinstance(message, SystemMessage):
                pass  # keep spinner running
    except MessageParseError as e:
        _stop_spinner()
        _console.print(f"[dim]Skipped unknown message: {e}[/dim]")
    except Exception as e:
        _stop_spinner()
        _console.print(f"[bold red]error:[/bold red] {e}")

    return session_id


async def _safe_receive(client):
    """Iterate receive_response(), skipping unknown message types like rate_limit_event."""
    from claude_agent_sdk import ResultMessage
    from claude_agent_sdk._internal.message_parser import MessageParseError, parse_message

    async for raw_data in client._query.receive_messages():
        try:
            message = parse_message(raw_data)
        except MessageParseError:
            continue
        yield message
        if isinstance(message, ResultMessage):
            return


def _read_input() -> str | None:
    """Read user input from terminal."""
    try:
        return input("\033[1;32m\u276f \033[0m")
    except EOFError:
        return None


def _display_tool_use(block, console: Console | None = None) -> None:
    """Show compact tool activity indicator."""
    con = console or _console
    name = block.name
    tool_input = block.input or {}

    if name == "Read":
        path = tool_input.get("file_path", "")
        short = Path(path).name if path else "?"
        label = f"  [reading {short}]"
    elif name == "Write":
        path = tool_input.get("file_path", "")
        short = Path(path).name if path else "?"
        label = f"  [writing {short}]"
    elif name == "Edit":
        path = tool_input.get("file_path", "")
        short = Path(path).name if path else "?"
        label = f"  [editing {short}]"
    elif name == "Bash":
        cmd = tool_input.get("command", "")
        if len(cmd) > 60:
            cmd = cmd[:57] + "..."
        label = f"  [running: {cmd}]"
    elif name in ("Glob", "Grep"):
        pattern = tool_input.get("pattern", "")
        label = f"  [searching: {pattern}]"
    else:
        label = f"  [{name}]"

    con.print(Text(label, style="dim"))
