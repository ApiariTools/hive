"""hive next - Show highest priority action."""

import typer

from hive.core.context import get_context
from hive.core.hive_state import load_state
from hive.core.signals import prioritize


def next_action(
    ctx: typer.Context,
) -> None:
    """Show the highest priority action based on quests and signals."""
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)
    state = load_state(context.hive_dir)

    # Prioritize unacknowledged signals
    unacked = state.unacknowledged_signals()
    sorted_signals = prioritize(unacked)

    # Check for at-risk quests
    at_risk = [q for q in state.quests.values() if q.status == "at_risk"]
    blocked = [q for q in state.quests.values() if q.status == "blocked"]
    in_progress = [q for q in state.quests.values() if q.status == "in_progress"]
    planning = [q for q in state.quests.values() if q.status == "planning"]

    if not state.quests and not sorted_signals:
        typer.echo("No quests or signals. Create a quest with: hive plan \"...\"")
        return

    # Priority 1: Critical/high signals
    urgent_signals = [s for s in sorted_signals if s.severity in ("critical", "high")]
    if urgent_signals:
        s = urgent_signals[0]
        typer.secho(f"URGENT: {s.title}", fg="red", bold=True)
        typer.echo(f"  Source: {s.source} ({s.type}), severity: {s.severity}")
        if s.url:
            typer.echo(f"  {s.url}")
        if s.quest_id:
            quest = state.get_quest(s.quest_id)
            if quest:
                typer.echo(f"  Quest: #{quest.id} {quest.title}")
        return

    # Priority 2: At-risk quests
    if at_risk:
        q = at_risk[0]
        typer.secho(f"AT RISK: Quest #{q.id} {q.title}", fg="yellow", bold=True)
        open_signals = [s for s in q.signals if not s.acknowledged]
        if open_signals:
            typer.echo(f"  {len(open_signals)} unacknowledged signal(s)")
        return

    # Priority 3: Blocked quests
    if blocked:
        q = blocked[0]
        typer.secho(f"BLOCKED: Quest #{q.id} {q.title}", fg="yellow")
        blocked_items = [w for w in q.work_items if w.status == "blocked"]
        for item in blocked_items:
            typer.echo(f"  - {item.id}: {item.type} ({item.repo})")
        return

    # Priority 4: Medium/low signals
    if sorted_signals:
        s = sorted_signals[0]
        typer.echo(f"Signal: {s.title}")
        typer.echo(f"  Source: {s.source} ({s.type}), severity: {s.severity}")
        if s.url:
            typer.echo(f"  {s.url}")
        return

    # Priority 5: Continue in-progress work
    if in_progress:
        q = in_progress[0]
        typer.echo(f"Continue: Quest #{q.id} {q.title}")
        active = [w for w in q.work_items if w.status == "in_progress"]
        if active:
            typer.echo(f"  {len(active)} work item(s) in progress")
        return

    # Priority 6: Start planning work
    if planning:
        q = planning[0]
        typer.echo(f"Plan: Quest #{q.id} {q.title}")
        typer.echo("  No work items yet - break this quest into tasks.")
        return

    typer.echo("All quests are done. Create a new quest with: hive plan \"...\"")
