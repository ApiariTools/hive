"""Dashboard data collection and Rich rendering for hive menu and hive status."""

import json
import subprocess
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import dataclass, field
from datetime import datetime, timezone

from rich.console import Group
from rich.table import Table
from rich.text import Text

from hive.core.context import WorkspaceContext
from hive.core.hive_state import load_state as load_hive_state
from hive.core.reminders import Reminder, pending_reminders
from hive.core.signals import prioritize
from hive.core.tmux import list_windows, session_exists
from hive.models.quest import Quest, Signal


@dataclass
class Worker:
    """Active worker window."""

    window_name: str
    task_id: str
    repo: str


@dataclass
class PullRequest:
    """Open pull request."""

    number: int
    title: str
    repo: str
    branch: str
    url: str
    draft: bool = False


@dataclass
class DashboardData:
    """All data needed to render the dashboard."""

    quests: dict[str, Quest] = field(default_factory=dict)
    signals: list[Signal] = field(default_factory=list)
    workers: list[Worker] = field(default_factory=list)
    prs: list[PullRequest] = field(default_factory=list)
    reminders: list[Reminder] = field(default_factory=list)


# ---------------------------------------------------------------------------
# Data collection
# ---------------------------------------------------------------------------


def collect_state(context: WorkspaceContext) -> DashboardData:
    """Collect cheap state: quests, signals, workers, reminders. ~5ms."""
    state = load_hive_state(context.hive_dir)

    # Workers from tmux
    workers = _get_workers(context.session_name)

    # Pending reminders
    reminders = pending_reminders(context.hive_dir)

    # Unacknowledged signals, prioritized
    unacked = state.unacknowledged_signals()
    signals = prioritize(unacked)

    return DashboardData(
        quests=state.quests,
        signals=signals,
        workers=workers,
        reminders=reminders,
    )


def collect_prs(context: WorkspaceContext) -> list[PullRequest]:
    """Collect PRs from all repos via gh CLI. ~2-5s."""
    prs: list[PullRequest] = []

    if not context.has_workspace():
        return prs

    try:
        workspace = context.load_workspace()
    except Exception:
        return prs

    def fetch_repo_prs(repo_key: str, repo_path) -> list[PullRequest]:
        result_prs: list[PullRequest] = []
        if not repo_path.exists():
            return result_prs
        try:
            pr_fields = "number,title,headRefName,url,isDraft"
            pr_cmd = [
                "gh", "pr", "list",
                "--json", pr_fields,
                "--limit", "5",
            ]
            result = subprocess.run(
                pr_cmd,
                cwd=repo_path,
                capture_output=True,
                text=True,
                timeout=5,
            )
            if result.returncode == 0 and result.stdout.strip():
                pr_data = json.loads(result.stdout)
                for pr in pr_data:
                    result_prs.append(PullRequest(
                        number=pr["number"],
                        title=pr["title"],
                        repo=repo_key,
                        branch=pr["headRefName"],
                        url=pr["url"],
                        draft=pr.get("isDraft", False),
                    ))
        except (subprocess.TimeoutExpired, json.JSONDecodeError, FileNotFoundError):
            pass
        return result_prs

    with ThreadPoolExecutor(max_workers=8) as executor:
        futures = {
            executor.submit(fetch_repo_prs, key, cfg.path): key
            for key, cfg in workspace.repos.items()
        }
        for future in as_completed(futures, timeout=10):
            try:
                prs.extend(future.result())
            except Exception:
                pass

    return prs


def _get_workers(session_name: str) -> list[Worker]:
    """Get active worker windows from tmux."""
    workers: list[Worker] = []

    if not session_exists(session_name):
        return workers

    windows = list_windows(session_name)
    for window in windows:
        if "-" in window and window not in ("hive", "shell"):
            parts = window.split("-", 1)
            if len(parts) == 2:
                task_id, repo = parts
                workers.append(Worker(
                    window_name=window,
                    task_id=task_id,
                    repo=repo,
                ))

    return workers


# ---------------------------------------------------------------------------
# Rendering — shared helpers
# ---------------------------------------------------------------------------

STATUS_STYLE = {
    "at_risk": "bold red",
    "blocked": "bold yellow",
    "in_progress": "bold green",
    "done": "bold cyan",
    "planning": "dim",
}

STATUS_SHORT = {
    "at_risk": "AR",
    "blocked": "BL",
    "in_progress": "IP",
    "done": "DN",
    "planning": "PL",
}

SEVERITY_STYLE = {
    "critical": "bold red",
    "high": "red",
    "medium": "yellow",
    "low": "dim",
}


def _progress_bar(done: int, total: int, width: int = 10) -> Text:
    """Render a mini progress bar like ████████░░."""
    if total == 0:
        return Text("—", style="dim")
    filled = round(done / total * width)
    bar = "█" * filled + "░" * (width - filled)
    style = "green" if done == total else "yellow"
    text = Text(bar, style=style)
    text.append(f" {done}/{total}", style="dim")
    return text


# ---------------------------------------------------------------------------
# Rendering — tables
# ---------------------------------------------------------------------------


def render_quests_table(quests: dict[str, Quest], max_title: int = 40) -> Table:
    """Render quests as a Rich Table."""
    table = Table(show_header=False, show_edge=False, pad_edge=False, padding=(0, 1))
    table.add_column("id", style="dim", no_wrap=True)
    table.add_column("title", no_wrap=True, max_width=max_title)
    table.add_column("progress", no_wrap=True)
    table.add_column("status", no_wrap=True)

    for qid, quest in quests.items():
        done = sum(1 for w in quest.work_items if w.status == "done")
        total = len(quest.work_items)
        status_label = quest.status.upper()
        style = STATUS_STYLE.get(quest.status, "")

        table.add_row(
            f"#{qid}",
            quest.title[:max_title],
            _progress_bar(done, total),
            Text(status_label, style=style),
        )

    return table


def render_signals_table(signals: list[Signal], limit: int = 10) -> Table:
    """Render unacknowledged signals as a Rich Table."""
    table = Table(show_header=False, show_edge=False, pad_edge=False, padding=(0, 1))
    table.add_column("severity", no_wrap=True, width=5)
    table.add_column("title")
    table.add_column("source", style="dim", no_wrap=True)

    for signal in signals[:limit]:
        sev_style = SEVERITY_STYLE.get(signal.severity, "")
        table.add_row(
            Text(signal.severity.upper()[:4], style=sev_style),
            signal.title,
            signal.source,
        )

    return table


def render_workers_table(workers: list[Worker]) -> Table:
    """Render active workers as a Rich Table."""
    table = Table(show_header=False, show_edge=False, pad_edge=False, padding=(0, 1))
    table.add_column("name", no_wrap=True)
    table.add_column("branch", style="dim", no_wrap=True)

    for worker in workers:
        branch = f"feat/{worker.task_id}-{worker.repo}"
        table.add_row(worker.window_name, branch)

    return table


def render_prs_table(prs: list[PullRequest]) -> Table:
    """Render pull requests as a Rich Table."""
    table = Table(show_header=False, show_edge=False, pad_edge=False, padding=(0, 1))
    table.add_column("repo", style="dim", no_wrap=True)
    table.add_column("number", no_wrap=True)
    table.add_column("title")
    table.add_column("flags", no_wrap=True)

    for pr in prs:
        flags = Text("[draft]", style="dim") if pr.draft else Text("")
        table.add_row(
            pr.repo,
            f"#{pr.number}",
            pr.title,
            flags,
        )

    return table


# ---------------------------------------------------------------------------
# Rendering — composite views
# ---------------------------------------------------------------------------


def render_status(data: DashboardData) -> Group:
    """Full-width rendering for `hive status`."""
    parts: list = []

    if data.quests:
        parts.append(Text("Quests", style="bold"))
        parts.append(render_quests_table(data.quests))
        parts.append(Text())

    if data.signals:
        parts.append(Text(f"Signals ({len(data.signals)} unacknowledged)", style="bold"))
        parts.append(render_signals_table(data.signals))
        parts.append(Text())

    if data.reminders:
        parts.append(Text(f"Reminders ({len(data.reminders)} pending)", style="bold"))
        for r in data.reminders[:5]:
            due = _format_due(r.due_at)
            parts.append(Text(f"  {due}  {r.message}", style="dim"))
        parts.append(Text())

    if data.workers:
        parts.append(Text(f"Workers ({len(data.workers)})", style="bold"))
        parts.append(render_workers_table(data.workers))
        parts.append(Text())

    if data.prs:
        parts.append(Text(f"Pull Requests ({len(data.prs)})", style="bold"))
        parts.append(render_prs_table(data.prs))
        parts.append(Text())

    if not parts:
        parts.append(Text("No quests or signals yet.", style="dim"))
        parts.append(Text())
        parts.append(Text("Get started:"))
        parts.append(Text('  hive plan "Launch feature X"  - Create a quest'))
        parts.append(Text("  hive heartbeat --once          - Poll for signals"))

    return Group(*parts)


def render_dashboard(data: DashboardData) -> Group:
    """Compact rendering for `hive menu` (narrow pane ~55 chars)."""
    parts: list = []

    # Header with clock
    now = datetime.now(timezone.utc).strftime("%H:%M:%S")
    header = Text()
    header.append("HIVE", style="bold")
    header.append(f"  {now}", style="dim")
    parts.append(header)
    parts.append(Text("━" * 40, style="dim"))

    # Quests (compact)
    if data.quests:
        parts.append(Text("Quests", style="bold"))
        for qid, quest in data.quests.items():
            done = sum(1 for w in quest.work_items if w.status == "done")
            total = len(quest.work_items)
            short = STATUS_SHORT.get(quest.status, "??")
            style = STATUS_STYLE.get(quest.status, "")
            line = Text(f"  #{qid} ")
            line.append(quest.title[:30], style="")
            line.append(f"  {done}/{total} ", style="dim")
            line.append(short, style=style)
            parts.append(line)
        parts.append(Text())

    # Signals (compact)
    if data.signals:
        parts.append(Text(f"Signals ({len(data.signals)})", style="bold"))
        for signal in data.signals[:5]:
            sev_style = SEVERITY_STYLE.get(signal.severity, "")
            line = Text("  ")
            line.append(signal.severity.upper()[:4], style=sev_style)
            line.append(f" {signal.title[:35]}")
            parts.append(line)
        parts.append(Text())

    # Workers (compact)
    if data.workers:
        parts.append(Text(f"Workers ({len(data.workers)})", style="bold"))
        for worker in data.workers:
            parts.append(Text(f"  {worker.window_name}", style=""))
        parts.append(Text())

    # PRs (compact)
    if data.prs:
        parts.append(Text(f"PRs ({len(data.prs)})", style="bold"))
        for pr in data.prs:
            draft = " [draft]" if pr.draft else ""
            line = Text(f"  {pr.repo} ")
            line.append(f"#{pr.number}", style="dim")
            line.append(f" {pr.title[:25]}{draft}")
            parts.append(line)
        parts.append(Text())

    if not data.quests and not data.signals and not data.workers and not data.prs:
        parts.append(Text())
        parts.append(Text("No activity yet.", style="dim"))

    return Group(*parts)


def _format_due(due_at: str) -> str:
    """Format a due_at timestamp as relative time."""
    try:
        due = datetime.fromisoformat(due_at.replace("Z", "+00:00"))
        now = datetime.now(timezone.utc)
        delta = due - now
        secs = int(delta.total_seconds())
        if secs < 0:
            return "overdue"
        if secs < 60:
            return f"in {secs}s"
        if secs < 3600:
            return f"in {secs // 60}m"
        if secs < 86400:
            return f"in {secs // 3600}h"
        return f"in {secs // 86400}d"
    except (ValueError, TypeError):
        return "?"
