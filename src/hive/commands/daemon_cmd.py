"""hive daemon - Manage all daemons (coordinator + workers)."""

import asyncio
import json
from pathlib import Path

import typer

from hive.core.context import get_context

daemon_app = typer.Typer(
    name="daemon",
    help="Manage all hive daemons (coordinator + workers).",
)


def _list_daemon_names(hive_dir: Path) -> list[str]:
    """Return daemon names: coordinator (if present) + all workers."""
    names: list[str] = []

    # Coordinator
    cdir = hive_dir / "coordinator"
    if cdir.is_dir():
        names.append("coordinator")

    # Workers
    workers_dir = hive_dir / "workers"
    if workers_dir.is_dir():
        for d in sorted(workers_dir.iterdir()):
            if d.is_dir():
                names.append(f"workers/{d.name}")

    return names


@daemon_app.command("status")
def daemon_status_cmd(ctx: typer.Context) -> None:
    """Show status of all daemons (coordinator + workers)."""
    from hive.core.daemon import daemon_status

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    names = _list_daemon_names(context.hive_dir)
    if not names:
        typer.echo("No daemons found.")
        return

    typer.echo(f"{'NAME':<30} {'STATE':<12} {'PID':<10} {'UPDATED'}")
    typer.echo("─" * 70)

    for name in names:
        info = daemon_status(name, context.hive_dir)
        state = info.get("state", "unknown")
        running = info.get("running", False)
        pid = info.get("pid", "")
        updated = info.get("updated_at", "")
        if updated and len(updated) > 19:
            updated = updated[:19].replace("T", " ")

        display_name = name.replace("workers/", "worker:")
        color = "green" if running else "yellow"
        typer.secho(
            f"{display_name:<30} {state:<12} {str(pid):<10} {updated}",
            fg=color,
        )


@daemon_app.command("stop")
def daemon_stop_cmd(ctx: typer.Context) -> None:
    """Stop all running daemons."""
    from hive.core.daemon import stop_daemon

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    names = _list_daemon_names(context.hive_dir)
    stopped = 0

    for name in names:
        if stop_daemon(name, context.hive_dir):
            display_name = name.replace("workers/", "worker:")
            typer.echo(f"  Stopped {display_name}")
            stopped += 1

    if stopped:
        typer.secho(f"Stopped {stopped} daemon(s).", fg="green")
    else:
        typer.echo("No running daemons to stop.")


@daemon_app.command("restart")
def daemon_restart_cmd(
    ctx: typer.Context,
    model: str | None = typer.Option(
        None, "--model", "-m", help="Override model for all daemons.",
    ),
) -> None:
    """Restart all daemons that were running.

    Stops everything first, then relaunches using saved config.json files.
    Useful after updating hive to pick up new code.
    """
    from hive.core.daemon import is_daemon_running, stop_daemon

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    names = _list_daemon_names(context.hive_dir)

    # Capture which daemons are currently running + their configs
    to_restart: list[dict] = []
    for name in names:
        if not is_daemon_running(name, context.hive_dir):
            continue
        config_path = context.hive_dir / name / "config.json"
        config: dict = {}
        if config_path.exists():
            try:
                config = json.loads(config_path.read_text())
            except (json.JSONDecodeError, OSError):
                pass
        to_restart.append({"name": name, "config": config})

    if not to_restart:
        typer.echo("No running daemons to restart.")
        return

    # Stop all
    for entry in to_restart:
        stop_daemon(entry["name"], context.hive_dir)
        display_name = entry["name"].replace("workers/", "worker:")
        typer.echo(f"  Stopped {display_name}")

    typer.echo()

    # Restart coordinator
    coordinator_entries = [e for e in to_restart if e["name"] == "coordinator"]
    if coordinator_entries:
        _restart_coordinator(context, coordinator_entries[0]["config"], model)

    # Restart workers
    worker_entries = [e for e in to_restart if e["name"].startswith("workers/")]
    for entry in worker_entries:
        worker_name = entry["name"].removeprefix("workers/")
        _restart_worker(context, worker_name, entry["config"], model)

    typer.secho(f"\nRestarted {len(to_restart)} daemon(s).", fg="green")


def _restart_coordinator(context, config: dict, model_override: str | None) -> None:
    """Relaunch the coordinator daemon."""
    from hive.core.chat_prompt import build_system_prompt
    from hive.core.daemon import start_daemon
    from hive.core.defaults import ensure_default_files

    context.ensure_dirs()
    ensure_default_files(context.hive_dir)

    system_prompt = build_system_prompt(context)
    effective_model = model_override or config.get("model")

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
                model=effective_model,
                console=console,
            )
        )

    # Update persisted config
    config_path = context.hive_dir / "coordinator" / "config.json"
    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(json.dumps({"model": effective_model}))

    pid = start_daemon("coordinator", context.hive_dir, _coordinator_main)
    typer.secho(f"  Started coordinator (pid={pid})", fg="green")


def _restart_worker(
    context, worker_name: str, config: dict, model_override: str | None,
) -> None:
    """Relaunch a single worker daemon."""
    from hive.core.daemon import start_daemon
    from hive.core.worker_session import run_worker_loop, worker_dir

    cwd = Path(config.get("cwd", "."))
    effective_model = model_override or config.get("model")

    # Rebuild system prompt from CLAUDE.local.md in the worker's cwd
    claude_local = cwd / "CLAUDE.local.md"
    system_prompt = ""
    if claude_local.exists():
        try:
            system_prompt = claude_local.read_text().strip()
        except OSError:
            pass

    daemon_name = f"workers/{worker_name}"

    def _worker_main():
        from hive.core.daemon import setup_daemon_logging

        wdir = worker_dir(context.hive_dir, worker_name)
        log_path = wdir / "output.log"
        console = setup_daemon_logging(log_path)

        asyncio.run(
            run_worker_loop(
                name=worker_name,
                system_prompt=system_prompt,
                initial_task=(
                    "Read TASK.md to understand your task."
                    " Review the linked issues for full context"
                    " and requirements. Then begin implementing"
                    " the changes needed in this repo."
                ),
                cwd=cwd,
                hive_dir=context.hive_dir,
                resume=True,
                model=effective_model,
                console=console,
                daemon_mode=True,
            )
        )

    # Update persisted config
    config_path = context.hive_dir / "workers" / worker_name / "config.json"
    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(json.dumps({"cwd": str(cwd), "model": effective_model}))

    pid = start_daemon(daemon_name, context.hive_dir, _worker_main)
    typer.secho(f"  Started worker:{worker_name} (pid={pid})", fg="green")
