"""hive ask - Chat interface over hive state."""

import json
from typing import Annotated

import typer

from hive.core.context import get_context
from hive.core.hive_state import load_state
from hive.core.signals import prioritize


def ask(
    ctx: typer.Context,
    topic: Annotated[
        str,
        typer.Argument(help="Topic to ask about (e.g. 'heartbeat', 'quests', 'signals')."),
    ],
    question: Annotated[
        str,
        typer.Argument(help="Your question."),
    ] = "",
) -> None:
    """Ask questions about hive state (quests, signals, heartbeat)."""
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)
    state = load_state(context.hive_dir)

    topic = topic.lower()

    if topic == "heartbeat":
        _answer_heartbeat(state, question)
    elif topic in ("quests", "quest"):
        _answer_quests(state, question)
    elif topic in ("signals", "signal"):
        _answer_signals(state, question)
    elif topic == "state":
        _answer_state(state)
    else:
        typer.echo(f"Unknown topic: {topic}")
        typer.echo("Available topics: heartbeat, quests, signals, state")


def _answer_heartbeat(state, question: str) -> None:
    """Answer questions about heartbeat state."""
    cursors = state.watcher_cursors
    if not cursors:
        typer.echo("No heartbeat data yet. Run: hive heartbeat --once")
        return

    typer.echo("Heartbeat watcher cursors:")
    for name, cursor in cursors.items():
        typer.echo(f"  {name}: {json.dumps(cursor)}")

    unacked = state.unacknowledged_signals()
    typer.echo(f"\nUnacknowledged signals: {len(unacked)}")
    if unacked:
        sorted_signals = prioritize(unacked)
        for s in sorted_signals[:5]:
            typer.echo(f"  [{s.severity}] {s.title} ({s.source})")


def _answer_quests(state, question: str) -> None:
    """Answer questions about quests."""
    if not state.quests:
        typer.echo("No quests. Create one with: hive plan \"...\"")
        return

    for qid, quest in state.quests.items():
        typer.echo(f"Quest #{qid}: {quest.title}")
        typer.echo(f"  Status: {quest.status}")
        typer.echo(f"  Goal: {quest.goal}")
        if quest.repos:
            typer.echo(f"  Repos: {', '.join(quest.repos)}")
        if quest.work_items:
            typer.echo(f"  Work items: {len(quest.work_items)}")
            for item in quest.work_items:
                typer.echo(f"    - [{item.status}] {item.id}: {item.type} ({item.repo})")
        if quest.signals:
            open_signals = [s for s in quest.signals if not s.acknowledged]
            typer.echo(f"  Open signals: {len(open_signals)}")
        typer.echo()


def _answer_signals(state, question: str) -> None:
    """Answer questions about signals."""
    if not state.signals:
        typer.echo("No signals. Run: hive heartbeat --once")
        return

    sorted_signals = prioritize(state.signals)
    unacked = [s for s in sorted_signals if not s.acknowledged]
    acked = [s for s in sorted_signals if s.acknowledged]

    typer.echo(f"Signals: {len(state.signals)} total, {len(unacked)} unacknowledged")
    typer.echo()

    if unacked:
        typer.echo("Unacknowledged:")
        for s in unacked[:20]:
            quest_info = f" (Quest #{s.quest_id})" if s.quest_id else ""
            typer.echo(f"  [{s.severity}] {s.title} ({s.source}/{s.type}){quest_info}")
            if s.url:
                typer.echo(f"       {s.url}")

    if acked:
        typer.echo(f"\nAcknowledged: {len(acked)}")


def _answer_state(state) -> None:
    """Dump full state as JSON."""
    typer.echo(json.dumps(state.to_dict(), indent=2))
