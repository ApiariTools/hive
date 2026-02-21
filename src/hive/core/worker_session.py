"""Worker session using claude-agent-sdk for autonomous task execution.

Workers run as daemons (or in foreground) and communicate via file-based IPC:
  - .hive/workers/<name>/session.json   — session ID for resume
  - .hive/workers/<name>/history.jsonl  — append-only conversation log
  - .hive/workers/<name>/inbox.jsonl    — messages from main chat
  - .hive/workers/<name>/status.json    — daemon state heartbeat
  - .hive/workers/<name>/output.log     — daemon stdout/stderr
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

_console = Console(highlight=False)

_SPINNER_FRAMES = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"


class _Spinner:
    """Animated spinner on stdout. No-op when running as daemon."""

    def __init__(self, message: str = "working", *, daemon_mode: bool = False):
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


# ---------------------------------------------------------------------------
# File-based IPC helpers
# ---------------------------------------------------------------------------


def worker_dir(hive_dir: Path, name: str) -> Path:
    """Get worker directory: .hive/workers/<name>/."""
    return hive_dir / "workers" / name


def _load_session_id(wdir: Path) -> str | None:
    path = wdir / "session.json"
    if not path.exists():
        return None
    try:
        with open(path) as f:
            data = json.load(f)
        return data.get("session_id")
    except (json.JSONDecodeError, OSError):
        return None


def _save_session(wdir: Path, session_id: str, name: str) -> None:
    wdir.mkdir(parents=True, exist_ok=True)
    path = wdir / "session.json"
    with open(path, "w") as f:
        json.dump({
            "session_id": session_id,
            "name": name,
            "started_at": datetime.now(timezone.utc).isoformat(),
        }, f)


def append_history(wdir: Path, role: str, content: str) -> None:
    """Append a message to history.jsonl."""
    wdir.mkdir(parents=True, exist_ok=True)
    path = wdir / "history.jsonl"
    entry = {
        "role": role,
        "content": content,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    with open(path, "a") as f:
        f.write(json.dumps(entry) + "\n")


def read_inbox(wdir: Path) -> list[dict]:
    """Read and clear inbox messages atomically."""
    path = wdir / "inbox.jsonl"
    if not path.exists():
        return []
    try:
        with open(path) as f:
            lines = f.readlines()
        if not lines:
            return []
        # Clear the inbox
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


def send_to_worker(hive_dir: Path, name: str, message: str) -> None:
    """Send a message to a worker's inbox."""
    wdir = worker_dir(hive_dir, name)
    wdir.mkdir(parents=True, exist_ok=True)
    path = wdir / "inbox.jsonl"
    entry = {
        "id": str(uuid4()),
        "message": message,
        "timestamp": datetime.now(timezone.utc).isoformat(),
    }
    with open(path, "a") as f:
        f.write(json.dumps(entry) + "\n")


def read_worker_history(
    hive_dir: Path, name: str, lines: int = 50,
) -> list[dict]:
    """Read recent history entries from a worker."""
    wdir = worker_dir(hive_dir, name)
    path = wdir / "history.jsonl"
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


def list_worker_sessions(hive_dir: Path) -> list[dict]:
    """List all worker sessions with metadata."""
    workers = hive_dir / "workers"
    if not workers.exists():
        return []
    sessions = []
    for d in sorted(workers.iterdir()):
        if not d.is_dir():
            continue
        info: dict = {"name": d.name}
        session_file = d / "session.json"
        if session_file.exists():
            try:
                with open(session_file) as f:
                    data = json.load(f)
                info["session_id"] = data.get("session_id")
                info["started_at"] = data.get("started_at")
            except (json.JSONDecodeError, OSError):
                pass
        sessions.append(info)
    return sessions


# ---------------------------------------------------------------------------
# Worker session loop
# ---------------------------------------------------------------------------


async def run_worker_loop(
    name: str,
    system_prompt: str,
    initial_task: str,
    cwd: Path,
    hive_dir: Path,
    resume: bool = True,
    model: str | None = None,
    poll_interval: float = 5.0,
    console: Console | None = None,
    daemon_mode: bool = False,
) -> None:
    """Run an autonomous worker session.

    The worker sends the initial task, displays output, then polls
    the inbox for follow-up messages from the main chat.

    Args:
        console: Rich Console to use for output (defaults to stdout).
        daemon_mode: If True, disables spinners and uses daemon-aware output.
    """
    from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient

    from hive.core.daemon import StatusHeartbeat, install_signal_handlers, write_status

    # Allow launching from within an existing Claude session
    os.environ.pop("CLAUDECODE", None)

    con = console or _console

    wdir = worker_dir(hive_dir, name)
    wdir.mkdir(parents=True, exist_ok=True)

    # Set up status heartbeat for daemon mode
    status_path = wdir / "status.json"
    heartbeat = StatusHeartbeat(status_path)

    # Install signal handlers for clean shutdown
    def _on_shutdown():
        write_status(status_path, "stopped", pid=os.getpid())

    install_signal_handlers(cleanup=_on_shutdown)

    heartbeat.set_state("starting", pid=os.getpid())

    session_id = _load_session_id(wdir) if resume else None

    options = ClaudeAgentOptions(
        system_prompt=system_prompt,
        cwd=str(cwd),
        permission_mode="bypassPermissions",
        model=model,
    )

    if session_id:
        options.resume = session_id
        con.print(
            f"[dim]Worker {name}: resuming {session_id[:8]}...[/dim]",
        )
    else:
        con.print(f"[dim]Worker {name}: starting...[/dim]")

    async with ClaudeSDKClient(options=options) as client:
        # Send initial task (skip if resuming — context already loaded)
        if not session_id:
            con.print(f"[dim]Task: {initial_task[:100]}[/dim]\n")
            append_history(wdir, "user", initial_task)

            heartbeat.set_state("running", pid=os.getpid())
            spinner = _Spinner("working", daemon_mode=daemon_mode)
            spinner.start()
            await client.query(initial_task)
            new_sid = await _receive_and_log(
                client, spinner, wdir, console=con,
            )
            if new_sid:
                session_id = new_sid
                _save_session(wdir, session_id, name)

        # Poll inbox for messages
        heartbeat.set_state("idle", pid=os.getpid())
        con.print(
            "\n[dim]Listening for messages... (Ctrl-C to stop)[/dim]\n",
        )

        while True:
            heartbeat.tick()
            messages = read_inbox(wdir)
            if messages:
                for msg in messages:
                    text = msg.get("message", "")
                    if not text:
                        continue

                    con.print()
                    con.print("[yellow]━━━ message received ━━━[/yellow]")
                    con.print(f"[yellow]{text}[/yellow]\n")

                    append_history(wdir, "user", text)

                    heartbeat.set_state("running", pid=os.getpid())
                    spinner = _Spinner("working", daemon_mode=daemon_mode)
                    spinner.start()
                    await client.query(text)
                    new_sid = await _receive_and_log(
                        client, spinner, wdir, console=con,
                    )
                    if new_sid:
                        session_id = new_sid
                        _save_session(wdir, session_id, name)
                    heartbeat.set_state("idle", pid=os.getpid())
            else:
                await asyncio.sleep(poll_interval)


async def _receive_and_log(
    client,
    spinner: _Spinner,
    wdir: Path,
    console: Console | None = None,
) -> str | None:
    """Receive response, display it, log to history, return session_id."""
    from claude_agent_sdk import AssistantMessage, ResultMessage, SystemMessage
    from claude_agent_sdk._internal.message_parser import (
        MessageParseError,
        parse_message,
    )
    from claude_agent_sdk.types import TextBlock, ToolUseBlock

    con = console or _console
    session_id = None
    response_parts: list[str] = []

    def _stop_spinner():
        spinner.stop()

    try:
        async for raw_data in client._query.receive_messages():
            try:
                message = parse_message(raw_data)
            except MessageParseError:
                continue

            if isinstance(message, AssistantMessage):
                for block in message.content:
                    if isinstance(block, TextBlock):
                        _stop_spinner()
                        con.print()
                        con.print("[cyan]\u25cf[/cyan]", end=" ")
                        con.print(Markdown(block.text.strip()))
                        response_parts.append(block.text.strip())
                    elif isinstance(block, ToolUseBlock):
                        _stop_spinner()
                        _display_tool_use(block, console=con)

            elif isinstance(message, ResultMessage):
                _stop_spinner()
                if message.is_error and message.result:
                    con.print(
                        f"[bold red]error:[/bold red] {message.result}",
                    )
                    response_parts.append(f"ERROR: {message.result}")
                session_id = message.session_id
                break

            elif isinstance(message, SystemMessage):
                pass  # keep spinner running

    except MessageParseError as e:
        _stop_spinner()
        con.print(f"[dim]Skipped unknown message: {e}[/dim]")
    except Exception as e:
        _stop_spinner()
        con.print(f"[bold red]error:[/bold red] {e}")

    # Log assistant response to history
    if response_parts:
        append_history(wdir, "assistant", "\n\n".join(response_parts))

    return session_id


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
