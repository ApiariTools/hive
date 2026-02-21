"""hive worker - Manage and communicate with SDK-based workers."""

import asyncio
import subprocess
from pathlib import Path
from typing import Annotated

import typer

from hive.core.context import get_context
from hive.core.dashboard import Worker, _get_workers
from hive.core.tmux import capture_pane, run_tmux, send_keys_to_pane
from hive.core.worker_session import (
    list_worker_sessions,
    read_worker_history,
    run_worker_loop,
    send_to_worker,
    worker_dir,
)

worker_app = typer.Typer(
    name="worker",
    help="Manage and communicate with active workers.",
)


def _resolve_worker(context, name: str) -> Worker | None:
    """Find a worker by exact name or substring match."""
    workers = _get_workers(context.session_name)
    # Exact match first
    for w in workers:
        if w.window_name == name:
            return w
    # Substring match
    matches = [w for w in workers if name in w.window_name]
    if len(matches) == 1:
        return matches[0]
    return None


def _infer_worker_name() -> str | None:
    """Infer worker name from cwd if inside a .hive/wt/<id>/<repo>/ worktree."""
    cwd = Path.cwd()
    parts = cwd.parts
    # Look for .hive/wt/<id>/<repo> in the path
    for i, part in enumerate(parts):
        if (
            part == "wt"
            and i >= 1
            and parts[i - 1] == ".hive"
            and i + 2 < len(parts)
        ):
            task_id = parts[i + 1]
            repo = parts[i + 2]
            return f"{task_id}-{repo}"
    return None


@worker_app.command("run")
def worker_run(
    ctx: typer.Context,
    name: Annotated[
        str | None,
        typer.Argument(help="Worker name (e.g. 42-backend). Inferred from cwd if omitted."),
    ] = None,
    task: Annotated[
        str | None,
        typer.Option("--task", "-t", help="Initial task (default: read TASK.md)."),
    ] = None,
    model: Annotated[
        str | None,
        typer.Option("--model", "-m", help="Model override."),
    ] = None,
    no_resume: Annotated[
        bool,
        typer.Option("--no-resume", help="Start fresh (don't resume)."),
    ] = False,
    foreground: Annotated[
        bool,
        typer.Option("--foreground", "-f", help="Run in foreground (don't daemonize)."),
    ] = False,
) -> None:
    """Run an SDK-managed worker session.

    Starts a Claude agent that works on the task in the current directory.
    Reads CLAUDE.local.md for system prompt and TASK.md for the task.
    Polls for messages sent via `hive worker send`.

    If NAME is omitted and you're inside a .hive/wt/<id>/<repo>/ worktree,
    the worker name is inferred automatically.

    By default, forks a background daemon. Use --foreground to run in the
    current terminal (useful for debugging).
    """
    if name is None:
        name = _infer_worker_name()
        if name is None:
            typer.secho(
                "Could not infer worker name from current directory.\n"
                "Either pass a name or run from inside a .hive/wt/<id>/<repo>/ worktree.",
                fg="red",
                err=True,
            )
            raise typer.Exit(1)
        typer.echo(f"Inferred worker name: {name}")
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

    cwd = Path.cwd()

    # Build system prompt from CLAUDE.local.md
    claude_local = cwd / "CLAUDE.local.md"
    system_prompt = ""
    if claude_local.exists():
        try:
            system_prompt = claude_local.read_text().strip()
        except OSError:
            pass

    # Default initial task
    if not task:
        task = (
            "Read TASK.md to understand your task."
            " Review the linked issues for full context"
            " and requirements. Then begin implementing"
            " the changes needed in this repo."
        )

    if foreground:
        # Run in foreground (current behavior)
        try:
            asyncio.run(
                run_worker_loop(
                    name=name,
                    system_prompt=system_prompt,
                    initial_task=task,
                    cwd=cwd,
                    hive_dir=context.hive_dir,
                    resume=not no_resume,
                    model=model,
                )
            )
        except KeyboardInterrupt:
            typer.echo(f"\nWorker {name} stopped.")
    else:
        # Daemon mode: fork and return
        from hive.core.daemon import is_daemon_running, start_daemon

        daemon_name = f"workers/{name}"

        if is_daemon_running(daemon_name, context.hive_dir):
            typer.secho(
                f"Worker {name} is already running. "
                "Use 'hive worker stop' first.",
                fg="yellow",
                err=True,
            )
            raise typer.Exit(1)

        def _worker_main():
            from hive.core.daemon import setup_daemon_logging

            wdir = worker_dir(context.hive_dir, name)
            log_path = wdir / "output.log"
            console = setup_daemon_logging(log_path)

            asyncio.run(
                run_worker_loop(
                    name=name,
                    system_prompt=system_prompt,
                    initial_task=task,
                    cwd=cwd,
                    hive_dir=context.hive_dir,
                    resume=not no_resume,
                    model=model,
                    console=console,
                    daemon_mode=True,
                )
            )

        # Persist config so `hive daemon restart` can relaunch
        config_path = context.hive_dir / "workers" / name / "config.json"
        config_path.parent.mkdir(parents=True, exist_ok=True)
        import json as _json

        config_path.write_text(_json.dumps({"cwd": str(cwd), "model": model}))

        pid = start_daemon(daemon_name, context.hive_dir, _worker_main)
        typer.secho(f"Worker {name} started (pid={pid})", fg="green")


@worker_app.command("stop")
def worker_stop(
    ctx: typer.Context,
    name: Annotated[
        str, typer.Argument(help="Worker name (e.g. 42-backend)."),
    ],
) -> None:
    """Stop a running worker daemon."""
    from hive.core.daemon import stop_daemon

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    daemon_name = f"workers/{name}"
    if stop_daemon(daemon_name, context.hive_dir):
        typer.secho(f"Worker {name} stopped.", fg="green")
    else:
        typer.secho(f"Worker {name} is not running.", fg="yellow", err=True)


@worker_app.command("status")
def worker_status(
    ctx: typer.Context,
    name: Annotated[
        str, typer.Argument(help="Worker name (e.g. 42-backend)."),
    ],
) -> None:
    """Show status of a worker daemon."""
    from hive.core.daemon import daemon_status

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    daemon_name = f"workers/{name}"
    info = daemon_status(daemon_name, context.hive_dir)

    state = info.get("state", "unknown")
    running = info.get("running", False)
    pid = info.get("pid")
    updated = info.get("updated_at", "")

    if running:
        typer.secho(f"Worker {name}: {state} (pid={pid})", fg="green")
    else:
        typer.secho(f"Worker {name}: {state}", fg="yellow")

    if updated:
        typer.echo(f"  Last updated: {updated}")


@worker_app.command("list")
def worker_list(ctx: typer.Context) -> None:
    """List active workers (tmux windows + SDK sessions + daemons)."""
    from hive.core.daemon import daemon_status

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    # Tmux workers
    workers = _get_workers(context.session_name)
    # SDK sessions
    sessions = list_worker_sessions(context.hive_dir)
    session_names = {s["name"] for s in sessions}

    if not workers and not sessions:
        typer.echo("No active workers.")
        return

    if workers:
        for w in workers:
            sdk = " [sdk]" if w.window_name in session_names else ""
            # Check daemon status
            info = daemon_status(f"workers/{w.window_name}", context.hive_dir)
            daemon_tag = ""
            if info.get("running"):
                state = info.get("state", "running")
                daemon_tag = f" [daemon:{state}]"
            typer.echo(
                f"  {w.window_name}  (task={w.task_id} repo={w.repo}){sdk}{daemon_tag}",
            )

    # Show SDK sessions without tmux windows
    orphaned = [s for s in sessions if s["name"] not in {w.window_name for w in workers}]
    if orphaned:
        for s in orphaned:
            sid = s.get("session_id", "?")[:8]
            info = daemon_status(f"workers/{s['name']}", context.hive_dir)
            daemon_tag = ""
            if info.get("running"):
                state = info.get("state", "running")
                daemon_tag = f" [daemon:{state}]"
            typer.echo(f"  {s['name']}  (session={sid}, no tmux window){daemon_tag}")


@worker_app.command("peek")
def worker_peek(
    ctx: typer.Context,
    name: Annotated[
        str, typer.Argument(help="Worker name (e.g. 42-backend)."),
    ],
    lines: Annotated[
        int,
        typer.Option("--lines", "-n", help="Number of history entries."),
    ] = 20,
    raw: Annotated[
        bool,
        typer.Option("--raw", help="Show raw tmux pane instead of history."),
    ] = False,
    follow: Annotated[
        bool,
        typer.Option("--follow", "-f", help="Tail daemon output.log live."),
    ] = False,
) -> None:
    """Show recent worker activity.

    Reads from SDK history by default. Use --raw for tmux pane capture.
    Use --follow / -f to tail the daemon's output.log live.
    """
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    # --follow: tail output.log
    if follow:
        wdir = worker_dir(context.hive_dir, name)
        log_path = wdir / "output.log"
        if not log_path.exists():
            typer.secho(f"No output.log for worker: {name}", fg="yellow", err=True)
            raise typer.Exit(1)
        try:
            subprocess.run(["tail", "-f", str(log_path)], check=False)
        except KeyboardInterrupt:
            pass
        return

    # --raw: fall back to tmux capture
    if raw:
        worker = _resolve_worker(context, name)
        if not worker:
            typer.secho(f"Worker not found in tmux: {name}", fg="red", err=True)
            raise typer.Exit(1)
        try:
            output = capture_pane(
                context.session_name,
                worker.window_name,
                pane_index=0,
                lines=lines,
            )
            typer.echo(output)
        except Exception as e:
            typer.secho(f"Failed to capture pane: {e}", fg="red", err=True)
            raise typer.Exit(1)
        return

    # SDK history
    history = read_worker_history(context.hive_dir, name, lines=lines)
    if not history:
        # Try tmux fallback
        worker = _resolve_worker(context, name)
        if worker:
            typer.echo("[no SDK history — showing tmux output]")
            try:
                output = capture_pane(
                    context.session_name,
                    worker.window_name,
                    pane_index=0,
                    lines=lines,
                )
                typer.echo(output)
            except Exception:
                typer.echo("(no output)")
        else:
            typer.echo(f"No history for worker: {name}")
        return

    for entry in history:
        role = entry.get("role", "?")
        content = entry.get("content", "")
        ts = entry.get("timestamp", "")
        time_short = ts[11:19] if len(ts) > 19 else ""

        if role == "user":
            typer.secho(f"[{time_short}] \u276f {content[:200]}", fg="green")
        elif role == "assistant":
            # Show first few lines of response
            preview = content[:500]
            if len(content) > 500:
                preview += "..."
            typer.echo(f"[{time_short}] \u25cf {preview}")
        else:
            typer.secho(f"[{time_short}] ({role})", dim=True)


@worker_app.command("send")
def worker_send(
    ctx: typer.Context,
    name: Annotated[
        str, typer.Argument(help="Worker name (e.g. 42-backend)."),
    ],
    message: Annotated[
        str, typer.Argument(help="Message to send to the worker."),
    ],
    raw: Annotated[
        bool,
        typer.Option("--raw", help="Send via tmux instead of inbox."),
    ] = False,
) -> None:
    """Send a message to a worker.

    Writes to the worker's inbox file by default. The worker picks it up
    on its next poll cycle (~5s). Use --raw to send directly via tmux.
    """
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    if raw:
        worker = _resolve_worker(context, name)
        if not worker:
            typer.secho(f"Worker not found: {name}", fg="red", err=True)
            raise typer.Exit(1)
        try:
            send_keys_to_pane(
                context.session_name,
                worker.window_name,
                pane_index=0,
                keys=message,
                enter=True,
            )
            typer.secho(f"Sent to {worker.window_name} (tmux)", fg="green")
        except Exception as e:
            typer.secho(f"Failed to send: {e}", fg="red", err=True)
            raise typer.Exit(1)
        return

    # Check if worker session exists
    wdir = worker_dir(context.hive_dir, name)
    if not wdir.exists():
        # Try fuzzy match against SDK sessions
        sessions = list_worker_sessions(context.hive_dir)
        matches = [s for s in sessions if name in s["name"]]
        if len(matches) == 1:
            name = matches[0]["name"]
        elif not matches:
            typer.secho(
                f"No SDK session for worker: {name}\n"
                "Use --raw to send via tmux instead.",
                fg="red",
                err=True,
            )
            raise typer.Exit(1)

    send_to_worker(context.hive_dir, name, message)
    typer.secho(f"Sent to {name} (inbox)", fg="green")


@worker_app.command("interrupt")
def worker_interrupt(
    ctx: typer.Context,
    name: Annotated[
        str, typer.Argument(help="Worker name (e.g. 42-backend)."),
    ],
    pane: Annotated[
        int,
        typer.Option("--pane", "-p", help="Pane index (0=claude, 1=shell)."),
    ] = 0,
) -> None:
    """Send Escape to a worker pane (interrupt current input)."""
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    worker = _resolve_worker(context, name)
    if not worker:
        typer.secho(f"Worker not found: {name}", fg="red", err=True)
        raise typer.Exit(1)

    try:
        target = f"{context.session_name}:{worker.window_name}.{pane}"
        run_tmux(["send-keys", "-t", target, "Escape"])
        typer.secho(f"Sent Escape to {worker.window_name}", fg="green")
    except Exception as e:
        typer.secho(f"Failed: {e}", fg="red", err=True)
        raise typer.Exit(1)


@worker_app.command("chat")
def worker_chat(
    ctx: typer.Context,
    name: Annotated[
        str | None,
        typer.Argument(
            help="Worker name (e.g. 42-backend). "
            "Inferred from cwd if omitted.",
        ),
    ] = None,
) -> None:
    """Interactive chat with a worker.

    Opens a live view of the worker's output (tool use, code changes,
    responses) and lets you send messages. Starts the daemon first if
    it's not already running.

    If NAME is omitted and you're inside a .hive/wt/<id>/<repo>/
    worktree, the name is inferred automatically.
    """
    import select
    import sys
    import termios
    import tty

    from hive.core.daemon import daemon_status, is_daemon_running

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    if name is None:
        name = _infer_worker_name()
        if name is None:
            typer.secho(
                "Could not infer worker name from current directory.\n"
                "Either pass a name or run from inside a "
                ".hive/wt/<id>/<repo>/ worktree.",
                fg="red",
                err=True,
            )
            raise typer.Exit(1)
        typer.echo(f"Inferred worker: {name}")

    daemon_name = f"workers/{name}"

    # Start daemon if not running
    if not is_daemon_running(daemon_name, context.hive_dir):
        typer.echo(f"Worker {name} is not running. Starting...")
        ctx.invoke(worker_run, name=name)

        if not is_daemon_running(daemon_name, context.hive_dir):
            typer.secho("Failed to start worker.", fg="red", err=True)
            raise typer.Exit(1)

    wdir = worker_dir(context.hive_dir, name)
    log_path = wdir / "output.log"

    typer.echo(
        f"Connected to worker {name}. "
        "Type a message + Enter to send. Ctrl-C to detach.\n",
    )

    # Open log file and seek to end (only show new output)
    log_file = open(log_path, "r") if log_path.exists() else None  # noqa: SIM115
    if log_file:
        log_file.seek(0, 2)  # seek to end

    # Use raw-ish terminal so we can multiplex log tailing + input
    fd = sys.stdin.fileno()
    old_settings = termios.tcgetattr(fd)
    input_buf = ""
    prompt = f"\033[1;32m[{name}] \u276f \033[0m"

    try:
        # cbreak mode: char-by-char input but still handle signals
        tty.setcbreak(fd)
        sys.stdout.write(prompt)
        sys.stdout.flush()

        while True:
            # Check if daemon is still alive periodically
            readable, _, _ = select.select(
                [sys.stdin] + ([log_file] if log_file else []),
                [], [], 0.2,
            )

            # Check for new log output
            if log_file:
                new_data = log_file.read()
                if new_data:
                    # Clear current input line, print log, redraw
                    sys.stdout.write(
                        f"\r\033[2K{new_data}",
                    )
                    # Redraw prompt + current input
                    sys.stdout.write(f"{prompt}{input_buf}")
                    sys.stdout.flush()
            elif log_path.exists():
                log_file = open(log_path, "r")  # noqa: SIM115
                log_file.seek(0, 2)

            # Check for user input
            if sys.stdin in readable:
                ch = sys.stdin.read(1)
                if ch == "\n" or ch == "\r":
                    sys.stdout.write("\n")
                    sys.stdout.flush()
                    stripped = input_buf.strip()
                    input_buf = ""

                    if not stripped:
                        sys.stdout.write(prompt)
                        sys.stdout.flush()
                        continue

                    if stripped == "/quit":
                        typer.echo("Disconnected.")
                        break
                    elif stripped == "/status":
                        info = daemon_status(
                            daemon_name, context.hive_dir,
                        )
                        state = info.get("state", "unknown")
                        running = info.get("running", False)
                        pid = info.get("pid")
                        if running:
                            typer.secho(
                                f"Worker {name}: {state} (pid={pid})",
                                fg="green",
                            )
                        else:
                            typer.secho(
                                f"Worker {name}: {state}", fg="yellow",
                            )
                    else:
                        send_to_worker(
                            context.hive_dir, name, stripped,
                        )
                        typer.secho(
                            f"Sent to {name}", fg="green", dim=True,
                        )

                    sys.stdout.write(prompt)
                    sys.stdout.flush()
                elif ch == "\x7f" or ch == "\x08":  # backspace
                    if input_buf:
                        input_buf = input_buf[:-1]
                        sys.stdout.write("\b \b")
                        sys.stdout.flush()
                elif ch == "\x03":  # Ctrl-C
                    raise KeyboardInterrupt
                elif ch >= " ":  # printable
                    input_buf += ch
                    sys.stdout.write(ch)
                    sys.stdout.flush()

    except KeyboardInterrupt:
        sys.stdout.write("\n")
        typer.echo("Detached from worker. (daemon still running)")
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)
        if log_file:
            log_file.close()
