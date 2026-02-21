"""hive chat - Persistent AI teammate conversation.

Supports two modes:
  - Foreground (--foreground): interactive terminal chat (legacy behavior)
  - Default: starts coordinator daemon if needed, runs thin terminal client
"""

import asyncio
import json
import sys
import time
from pathlib import Path
from typing import Annotated

import typer

from hive.core.context import get_context
from hive.core.exceptions import WorkspaceError


def chat(
    ctx: typer.Context,
    new: Annotated[
        bool,
        typer.Option("--new", help="Start a fresh session (don't resume)."),
    ] = False,
    model: Annotated[
        str | None,
        typer.Option("--model", "-m", help="Model override (e.g. sonnet, opus)."),
    ] = None,
    foreground: Annotated[
        bool,
        typer.Option("--foreground", help="Run interactive chat in foreground (no daemon)."),
    ] = False,
) -> None:
    """Talk to Hive like a teammate.

    Starts an interactive conversation with full access to your workspace,
    quests, signals, and all hive CLI commands.

    By default, starts a coordinator daemon and opens a thin terminal client.
    Use --foreground for the legacy interactive experience (no daemon).

    Resume previous conversations automatically, or use --new for a fresh start.
    """
    try:
        from claude_agent_sdk import ClaudeSDKClient  # noqa: F401
    except ImportError:
        typer.secho(
            "claude-agent-sdk is not installed.\n"
            "Install it with: pip install claude-agent-sdk",
            fg="red",
            err=True,
        )
        raise typer.Exit(1)

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    # Load workspace if available (non-fatal if missing)
    if context.has_workspace():
        try:
            context.load_workspace()
        except WorkspaceError:
            pass

    from hive.core.chat_prompt import build_system_prompt
    from hive.core.defaults import ensure_default_files

    context.ensure_dirs()
    ensure_default_files(context.hive_dir)

    system_prompt = build_system_prompt(context)

    if foreground:
        # Legacy: interactive terminal chat, no daemon
        from hive.core.chat_session import run_chat_loop

        while True:
            result = asyncio.run(
                run_chat_loop(
                    system_prompt=system_prompt,
                    cwd=context.root,
                    hive_dir=context.hive_dir,
                    resume_session=not new,
                    model=model,
                )
            )

            if result == "restart":
                system_prompt = build_system_prompt(context)
                new = True
                continue
            else:
                break
    else:
        # Daemon mode: ensure coordinator is running, then run thin client
        _ensure_coordinator_running(context, system_prompt, model, new)
        _run_chat_client(context.hive_dir)


def _ensure_coordinator_running(
    context,
    system_prompt: str,
    model: str | None,
    new: bool,
) -> None:
    """Start coordinator daemon if not already running."""
    from hive.core.daemon import is_daemon_running, start_daemon

    daemon_name = "coordinator"

    if new:
        # Force restart: stop existing daemon first
        from hive.core.daemon import stop_daemon

        if is_daemon_running(daemon_name, context.hive_dir):
            typer.echo("Stopping existing coordinator...")
            stop_daemon(daemon_name, context.hive_dir)
            # Clean session to force fresh start
            session_path = context.hive_dir / "coordinator" / "session.json"
            session_path.unlink(missing_ok=True)

    if is_daemon_running(daemon_name, context.hive_dir):
        typer.echo("Coordinator already running.")
        return

    def _coordinator_main():
        from hive.core.chat_session import run_coordinator_loop
        from hive.core.daemon import setup_daemon_logging

        cdir = context.hive_dir / "coordinator"
        log_path = cdir / "output.log"
        console = setup_daemon_logging(log_path)

        asyncio.run(
            run_coordinator_loop(
                system_prompt=system_prompt,
                cwd=context.root,
                hive_dir=context.hive_dir,
                model=model,
                console=console,
            )
        )

    # Persist config so `hive daemon restart` can relaunch
    config_path = context.hive_dir / "coordinator" / "config.json"
    config_path.parent.mkdir(parents=True, exist_ok=True)
    import json as _json

    config_path.write_text(_json.dumps({"model": model}))

    pid = start_daemon(daemon_name, context.hive_dir, _coordinator_main)
    typer.secho(f"Coordinator started (pid={pid})", fg="green")


def _run_chat_client(hive_dir: Path) -> None:
    """Thin terminal client that reads/writes coordinator files.

    - Displays recent history on connect
    - Reads user input, writes to inbox.jsonl
    - Tails history.jsonl for new responses
    """
    from hive.core.chat_session import read_coordinator_history, send_to_coordinator

    cdir = hive_dir / "coordinator"
    history_path = cdir / "history.jsonl"

    # Show recent history on connect
    history = read_coordinator_history(hive_dir, lines=10)
    if history:
        typer.echo("--- recent history ---")
        for entry in history:
            role = entry.get("role", "?")
            content = entry.get("content", "")
            ts = entry.get("timestamp", "")
            time_short = ts[11:19] if len(ts) > 19 else ""

            if role == "user":
                typer.secho(f"[{time_short}] \u276f {content[:200]}", fg="green")
            elif role == "assistant":
                preview = content[:500]
                if len(content) > 500:
                    preview += "..."
                typer.echo(f"[{time_short}] \u25cf {preview}")
        typer.echo("--- end history ---\n")

    typer.echo("Connected to coordinator. Type /quit to disconnect, /status for info.\n")

    # Track history file position for tailing
    history_pos = history_path.stat().st_size if history_path.exists() else 0

    while True:
        try:
            user_input = input("\033[1;32m\u276f \033[0m")
        except (EOFError, KeyboardInterrupt):
            typer.echo("\nDisconnected.")
            break

        stripped = user_input.strip()
        if not stripped:
            continue

        if stripped == "/quit":
            typer.echo("Disconnected.")
            break
        elif stripped == "/status":
            from hive.core.daemon import daemon_status

            info = daemon_status("coordinator", hive_dir)
            state = info.get("state", "unknown")
            running = info.get("running", False)
            pid = info.get("pid")
            if running:
                typer.secho(f"Coordinator: {state} (pid={pid})", fg="green")
            else:
                typer.secho(f"Coordinator: {state}", fg="yellow")
            continue
        elif stripped == "/new":
            typer.echo("Use 'hive chat --new' to restart the coordinator.")
            continue

        # Send message to coordinator inbox
        send_to_coordinator(hive_dir, stripped, source="terminal")

        # Wait for response by tailing history.jsonl
        _wait_for_response(history_path, history_pos)

        # Update position
        history_pos = history_path.stat().st_size if history_path.exists() else 0


def _wait_for_response(
    history_path: Path,
    start_pos: int,
    timeout: float = 300.0,
) -> None:
    """Wait for new entries in history.jsonl and display them."""
    deadline = time.monotonic() + timeout
    displayed_pos = start_pos
    got_response = False

    # Small initial delay to let daemon pick up the message
    time.sleep(0.3)

    while time.monotonic() < deadline:
        if not history_path.exists():
            time.sleep(0.5)
            continue

        current_size = history_path.stat().st_size
        if current_size > displayed_pos:
            try:
                with open(history_path) as f:
                    f.seek(displayed_pos)
                    new_data = f.read()
                displayed_pos = history_path.stat().st_size

                for line in new_data.strip().split("\n"):
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        entry = json.loads(line)
                    except json.JSONDecodeError:
                        continue

                    role = entry.get("role", "?")
                    content = entry.get("content", "")
                    if role == "assistant":
                        sys.stdout.write(f"\n\033[36m\u25cf\033[0m {content}\n\n")
                        sys.stdout.flush()
                        got_response = True
                    elif role == "user":
                        # Our own message echoed back — skip display
                        pass

                if got_response:
                    return
            except OSError:
                pass

        time.sleep(0.5)

    if not got_response:
        typer.secho("(timeout waiting for response)", fg="yellow")
