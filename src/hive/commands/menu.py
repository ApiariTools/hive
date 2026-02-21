"""hive menu - Interactive Rich dashboard for hive session."""

import os
import select
import subprocess
import sys
import termios
import time
import tty
from datetime import datetime, timezone

import typer
from rich.console import Console
from rich.text import Text

from hive.core.context import get_context
from hive.core.dashboard import (
    SEVERITY_STYLE,
    STATUS_SHORT,
    STATUS_STYLE,
    DashboardData,
    collect_prs,
    collect_state,
)
from hive.core.tmux import run_tmux

menu_app = typer.Typer(
    name="menu",
    help="Interactive dashboard for hive session.",
    no_args_is_help=False,
)


def _read_key(timeout: float = 5.0) -> str | None:
    """Read a single keypress with timeout. Returns None on timeout."""
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    try:
        tty.setraw(fd)
        ready, _, _ = select.select([fd], [], [], timeout)
        if not ready:
            return None
        ch = os.read(fd, 1).decode("utf-8", errors="ignore")
        if ch == "\x1b":
            ready2, _, _ = select.select([fd], [], [], 0.05)
            if ready2:
                ch2 = os.read(fd, 1).decode("utf-8", errors="ignore")
                if ch2 == "[":
                    ready3, _, _ = select.select([fd], [], [], 0.05)
                    if ready3:
                        ch3 = os.read(fd, 1).decode("utf-8", errors="ignore")
                        if ch3 == "A":
                            return "up"
                        if ch3 == "B":
                            return "down"
            return None
        return ch
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


def _available_sections(data: DashboardData) -> list[str]:
    """Return navigable sections that have items."""
    sections = []
    if data.quests:
        sections.append("quests")
    if data.workers:
        sections.append("workers")
    if data.prs:
        sections.append("prs")
    return sections


def _section_len(data: DashboardData, section: str) -> int:
    if section == "quests":
        return len(data.quests)
    if section == "workers":
        return len(data.workers)
    if section == "prs":
        return len(data.prs)
    return 0


def _open_url(url: str) -> None:
    try:
        subprocess.Popen(
            ["open", url],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except Exception:
        pass


def _issues_repo_url(context) -> str | None:
    """Get the GitHub URL prefix for the issues repo."""
    try:
        if not context.has_workspace():
            return None
        workspace = context.load_workspace()
        key = workspace.defaults.issues_repo
        if not key:
            return None
        repo = workspace.get_repo(key)
        if not repo or not repo.path.exists():
            return None
        result = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            cwd=repo.path,
            capture_output=True,
            text=True,
            timeout=2,
        )
        if result.returncode != 0:
            return None
        url = result.stdout.strip()
        if "github.com" not in url:
            return None
        if url.startswith("git@"):
            path = url.split(":", 1)[-1]
        elif "github.com/" in url:
            path = url.split("github.com/", 1)[-1]
        else:
            return None
        return f"https://github.com/{path.removesuffix('.git')}"
    except Exception:
        return None


def _render(
    console: Console,
    data: DashboardData,
    section: str,
    indices: dict[str, int],
    message: str = "",
) -> None:
    """Render the interactive dashboard."""
    # Clear screen and home cursor
    sys.stdout.write("\033[2J\033[H")
    sys.stdout.flush()

    now = datetime.now(timezone.utc).strftime("%H:%M:%S")
    header = Text(" HIVE", style="bold")
    header.append(f"  {now}", style="dim")
    console.print(header)
    console.print(" ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━", style="dim")

    # Quests
    if data.quests:
        active = section == "quests"
        label = "bold underline" if active else "bold"
        console.print(Text(" Quests", style=label))
        for i, (qid, quest) in enumerate(data.quests.items()):
            done = sum(1 for w in quest.work_items if w.status == "done")
            total = len(quest.work_items)
            short = STATUS_SHORT.get(quest.status, "??")
            st = STATUS_STYLE.get(quest.status, "")
            sel = active and i == indices.get("quests", 0)
            marker = ">" if sel else " "
            line = Text(f"  {marker} #{qid} ")
            line.append(quest.title[:28])
            line.append(f"  {done}/{total} ", style="dim")
            line.append(short, style=st)
            if sel:
                line.stylize("reverse")
            console.print(line)
        console.print()

    # Signals (display only)
    if data.signals:
        console.print(Text(f" Signals ({len(data.signals)})", style="bold"))
        for signal in data.signals[:5]:
            sev = SEVERITY_STYLE.get(signal.severity, "")
            line = Text("   ")
            line.append(signal.severity.upper()[:4], style=sev)
            line.append(f" {signal.title[:32]}")
            console.print(line)
        console.print()

    # Workers
    if data.workers:
        active = section == "workers"
        label = "bold underline" if active else "bold"
        console.print(
            Text(f" Workers ({len(data.workers)})", style=label),
        )
        for i, worker in enumerate(data.workers):
            sel = active and i == indices.get("workers", 0)
            marker = ">" if sel else " "
            line = Text(f"  {marker} {worker.window_name}")
            if sel:
                line.stylize("reverse")
            console.print(line)
        console.print()

    # PRs
    if data.prs:
        active = section == "prs"
        label = "bold underline" if active else "bold"
        console.print(Text(f" PRs ({len(data.prs)})", style=label))
        for i, pr in enumerate(data.prs):
            sel = active and i == indices.get("prs", 0)
            marker = ">" if sel else " "
            draft = " [d]" if pr.draft else ""
            line = Text(f"  {marker} {pr.repo} ")
            line.append(f"#{pr.number}", style="dim")
            line.append(f" {pr.title[:22]}{draft}")
            if sel:
                line.stylize("reverse")
            console.print(line)
        console.print()

    if (
        not data.quests
        and not data.signals
        and not data.workers
        and not data.prs
    ):
        console.print()
        console.print(Text(" No activity yet.", style="dim"))
        console.print()

    console.print(
        " ─────────────────────────────────────────",
        style="dim",
    )
    h = Text(" ")
    h.append("j/k", style="dim")
    h.append(" nav  ")
    h.append("Tab", style="dim")
    h.append(" section  ")
    h.append("Enter", style="dim")
    h.append(" open  ")
    h.append("r", style="dim")
    h.append(" refresh  ")
    h.append("q", style="dim")
    h.append(" quit")
    console.print(h)

    if message:
        console.print(Text(f" {message}", style="yellow"))


@menu_app.callback(invoke_without_command=True)
def menu_loop(ctx: typer.Context) -> None:
    """Interactive dashboard with keyboard navigation.

    Shows quests, signals, workers, and PRs. Auto-refreshes every 5s.
    Tab to switch sections, j/k to navigate, Enter to open.
    """
    if ctx.invoked_subcommand is not None:
        return

    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)
    console = Console()

    # Initial data load
    data = collect_state(context)
    data.prs = collect_prs(context)
    cached_prs = list(data.prs)
    last_pr_fetch = time.monotonic()

    # Resolve issues repo URL for quest links
    issues_url = _issues_repo_url(context)

    # Navigation state
    available = _available_sections(data)
    section = available[0] if available else "quests"
    indices: dict[str, int] = {"quests": 0, "workers": 0, "prs": 0}
    message = ""

    # Hide cursor
    sys.stdout.write("\033[?25l")
    sys.stdout.flush()

    try:
        while True:
            _render(console, data, section, indices, message)
            message = ""

            key = _read_key(timeout=5.0)

            if key is None:
                # Timeout — auto-refresh
                data = collect_state(context)
                if time.monotonic() - last_pr_fetch >= 60:
                    cached_prs = collect_prs(context)
                    last_pr_fetch = time.monotonic()
                data.prs = cached_prs
                continue

            if key in ("q", "\x03"):  # q or Ctrl-C
                break

            available = _available_sections(data)
            if not available:
                continue

            if key == "\t":
                if section in available:
                    idx = available.index(section)
                    section = available[(idx + 1) % len(available)]
                else:
                    section = available[0]

            elif key in ("j", "down"):
                max_idx = _section_len(data, section) - 1
                cur = indices.get(section, 0)
                if cur < max_idx:
                    indices[section] = cur + 1

            elif key in ("k", "up"):
                cur = indices.get(section, 0)
                if cur > 0:
                    indices[section] = cur - 1

            elif key == "\r":  # Enter
                if section == "prs" and data.prs:
                    idx = indices["prs"]
                    if 0 <= idx < len(data.prs):
                        pr = data.prs[idx]
                        _open_url(pr.url)
                        message = f"Opened #{pr.number}"
                elif section == "workers" and data.workers:
                    idx = indices["workers"]
                    if 0 <= idx < len(data.workers):
                        worker = data.workers[idx]
                        sname = context.session_name
                        try:
                            run_tmux([
                                "select-window", "-t",
                                f"{sname}:{worker.window_name}",
                            ])
                            message = f"→ {worker.window_name}"
                        except Exception:
                            message = "Failed to switch"
                elif section == "quests" and data.quests:
                    idx = indices["quests"]
                    keys = list(data.quests.keys())
                    if 0 <= idx < len(keys):
                        qid = keys[idx]
                        if issues_url:
                            _open_url(f"{issues_url}/issues/{qid}")
                            message = f"Opened #{qid}"

            elif key == "r":
                data = collect_state(context)
                cached_prs = collect_prs(context)
                data.prs = cached_prs
                last_pr_fetch = time.monotonic()
                message = "Refreshed"
                # Clamp indices
                for s in ("quests", "workers", "prs"):
                    max_idx = max(0, _section_len(data, s) - 1)
                    if indices.get(s, 0) > max_idx:
                        indices[s] = max_idx

    finally:
        sys.stdout.write("\033[?25h")  # Restore cursor
        sys.stdout.flush()
        sys.stdout.write("\033[2J\033[H")  # Clear screen
        sys.stdout.flush()
