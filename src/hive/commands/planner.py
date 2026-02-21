"""hive up - Start or attach to the hive tmux session."""

from typing import Annotated

import typer

from hive.core.context import get_context
from hive.core.exceptions import TmuxError, WorkspaceError
from hive.core.tmux import (
    attach_session,
    create_session,
    create_window,
    is_inside_tmux,
    run_tmux,
    session_exists,
    switch_client,
    window_exists,
)


def up(
    ctx: typer.Context,
    no_agent: Annotated[
        bool,
        typer.Option("--no-agent", help="Don't start Claude in the main window."),
    ] = False,
    no_menu: Annotated[
        bool,
        typer.Option("--no-menu", help="Don't start the interactive menu pane."),
    ] = False,
    no_web: Annotated[
        bool,
        typer.Option("--no-web", help="Don't start the web UI server."),
    ] = False,
) -> None:
    """Start or attach to the hive tmux session.

    Creates a persistent tmux session with:
    - Pane A (left): Claude Code
    - Pane B (right): Interactive dashboard (workers, issues, PRs)
    - A shell window for manual commands

    Re-opens task worker windows for any existing worktrees.
    """
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    # Load workspace for setup commands
    setup_commands: list[str] = []
    if context.has_workspace():
        try:
            workspace = context.load_workspace()
            setup_commands = workspace.defaults.worktree_setup_commands
        except WorkspaceError:
            pass

    try:
        if not session_exists(context.session_name):
            typer.echo("Creating hive session...")
            _create_session(
                context,
                start_agent=not no_agent,
                start_menu=not no_menu,
                start_web=not no_web,
            )
            # Reopen windows for existing worktrees
            reopened = _reopen_task_windows(context, setup_commands)
            if reopened:
                typer.echo(f"  Reopened {len(reopened)} task windows")
        else:
            typer.echo("Attaching to existing hive session...")
            # Check for any missing task windows
            reopened = _reopen_task_windows(context, setup_commands)
            if reopened:
                typer.echo(f"  Reopened {len(reopened)} task windows")

        # Always select main window before attaching
        run_tmux(["select-window", "-t", f"{context.session_name}:hive"])

        # Attach or switch
        if is_inside_tmux():
            switch_client(context.session_name)
        else:
            attach_session(context.session_name)

    except TmuxError as e:
        typer.secho(f"Error: {e}", fg="red", err=True)
        raise typer.Exit(1)


def _create_session(
    context,
    start_agent: bool = True,
    start_menu: bool = True,
    start_web: bool = True,
) -> None:
    """Create the hive tmux session with split panes."""
    target = f"{context.session_name}:hive"

    # Create session with main window
    create_session(
        session_name=context.session_name,
        start_dir=context.root,
        window_name="hive",
        detached=True,
    )

    # At this point we have one pane (the original)
    # Name it and start hive chat
    run_tmux(["select-pane", "-t", target, "-T", "claude"])
    if start_agent:
        run_tmux(["send-keys", "-t", target, "hive chat", "Enter"])

    if start_menu:
        # Split window horizontally - dashboard pane on right (35%)
        run_tmux([
            "split-window", "-t", target,
            "-h",  # horizontal split (side by side)
            "-p", "35",  # new pane is 35%
            "-c", str(context.root),
            "hive", "menu",  # unified dashboard
        ])
        run_tmux(["select-pane", "-T", "dashboard"])

        # Split for web server (tiny pane at bottom, 3 lines)
        if start_web:
            run_tmux([
                "split-window", "-t", target,
                "-v",  # vertical split
                "-l", "3",  # just 3 lines tall
                "-c", str(context.root),
                "hive", "web",  # web server
            ])
            run_tmux(["select-pane", "-T", "web"])

        # Select back to Claude pane (leftmost)
        run_tmux(["select-pane", "-t", target, "-L"])
    elif start_web:
        # No menu but still want web - add small pane on right
        run_tmux([
            "split-window", "-t", target,
            "-h",  # horizontal split
            "-l", "30",  # narrow
            "-c", str(context.root),
            "hive", "web",
        ])
        run_tmux(["select-pane", "-t", target, "-L"])

    # Create a shell window for manual commands
    create_window(
        session_name=context.session_name,
        window_name="shell",
        start_dir=context.root,
    )

    # Start heartbeat daemon in the shell window
    shell_target = f"{context.session_name}:shell"
    run_tmux([
        "split-window", "-t", shell_target,
        "-v",
        "-l", "3",
        "-c", str(context.root),
        "hive", "heartbeat", "--daemon",
    ])
    run_tmux(["select-pane", "-T", "heartbeat"])
    # Select back to the main shell pane
    run_tmux(["select-pane", "-t", shell_target, "-U"])

    # Go back to main window
    run_tmux(["select-window", "-t", target])

    typer.secho("Hive session created", fg="green")
    if start_agent:
        typer.echo("  Left pane: hive chat")
    if start_menu:
        typer.echo("  Right pane: Dashboard")
    if start_web:
        typer.echo("  Web UI: http://localhost:8080")
    typer.echo("  Shell window: manual commands")


def _reopen_task_windows(context, setup_commands: list[str]) -> list[str]:
    """Reopen tmux windows for existing task worktrees.

    Scans .hive/wt/ for task directories and creates windows
    for any that don't already exist.

    Args:
        context: Hive context.
        setup_commands: Commands to run in each new window (e.g. ["mise trust"]).

    Returns:
        List of window names that were created.
    """
    reopened = []
    worktrees_dir = context.worktrees_dir

    if not worktrees_dir.exists():
        return reopened

    # Scan for task directories
    for task_dir in worktrees_dir.iterdir():
        if not task_dir.is_dir():
            continue

        task_id = task_dir.name

        # Scan for repo worktrees within this task
        for repo_dir in task_dir.iterdir():
            if not repo_dir.is_dir():
                continue

            repo_key = repo_dir.name
            window_name = f"{task_id}-{repo_key}"

            # Skip if window already exists
            if window_exists(context.session_name, window_name):
                continue

            # Create window with setup commands running before shell starts
            try:
                shell_cmd = "; ".join(setup_commands) if setup_commands else None
                create_window(
                    session_name=context.session_name,
                    window_name=window_name,
                    start_dir=repo_dir,
                    shell_command=shell_cmd,
                )

                # Split window: main pane left, shell pane right (35%)
                target = f"{context.session_name}:{window_name}"
                run_tmux([
                    "split-window", "-t", target,
                    "-h",  # horizontal split
                    "-p", "35",  # shell pane is 35%
                    "-c", str(repo_dir),
                ])
                # Name the shell pane (currently selected after split)
                run_tmux(["select-pane", "-T", "shell"])
                # Select back to left pane and name it
                run_tmux(["select-pane", "-t", target, "-L"])
                run_tmux(["select-pane", "-T", "claude"])

                reopened.append(window_name)
            except TmuxError:
                # Non-fatal, continue with other windows
                pass

    return reopened
