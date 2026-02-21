"""Hive state management for quests, signals, and watcher cursors."""

import json
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from hive.models.quest import Quest, Signal


@dataclass
class HiveState:
    """Top-level state stored in .hive/state.json."""

    quests: dict[str, Quest] = field(default_factory=dict)
    signals: list[Signal] = field(default_factory=list)
    watcher_cursors: dict[str, dict[str, Any]] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "quests": {k: q.to_dict() for k, q in self.quests.items()},
            "signals": [s.to_dict() for s in self.signals],
            "watcher_cursors": self.watcher_cursors,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "HiveState":
        return cls(
            quests={k: Quest.from_dict(q) for k, q in data.get("quests", {}).items()},
            signals=[Signal.from_dict(s) for s in data.get("signals", [])],
            watcher_cursors=data.get("watcher_cursors", {}),
        )

    def get_quest(self, quest_id: str) -> Quest | None:
        return self.quests.get(quest_id)

    def add_quest(self, quest: Quest) -> None:
        self.quests[quest.id] = quest

    def unacknowledged_signals(self) -> list[Signal]:
        return [s for s in self.signals if not s.acknowledged]


def load_state(hive_dir: Path) -> HiveState:
    """Load state from .hive/state.json."""
    state_path = hive_dir / "state.json"
    if not state_path.exists():
        return HiveState()
    try:
        with open(state_path) as f:
            data = json.load(f)
        return HiveState.from_dict(data)
    except (json.JSONDecodeError, OSError, KeyError):
        return HiveState()


def save_state(hive_dir: Path, state: HiveState) -> None:
    """Save state to .hive/state.json."""
    hive_dir.mkdir(parents=True, exist_ok=True)
    state_path = hive_dir / "state.json"
    with open(state_path, "w") as f:
        json.dump(state.to_dict(), f, indent=2)


def append_event(hive_dir: Path, event: dict[str, Any]) -> None:
    """Append an event to .hive/events.log (one JSON object per line)."""
    hive_dir.mkdir(parents=True, exist_ok=True)
    events_path = hive_dir / "events.log"
    event["timestamp"] = datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
    with open(events_path, "a") as f:
        f.write(json.dumps(event) + "\n")


def save_quest_local(hive_dir: Path, quest: Quest) -> None:
    """Save per-quest local files under .hive/quests/<quest_id>/."""
    quest_dir = hive_dir / "quests" / quest.id
    quest_dir.mkdir(parents=True, exist_ok=True)

    # meta.json - quest metadata
    meta = {
        "id": quest.id,
        "title": quest.title,
        "goal": quest.goal,
        "definition_of_done": quest.definition_of_done,
        "repos": quest.repos,
        "metrics": quest.metrics,
        "updated_at": quest.updated_at,
    }
    with open(quest_dir / "meta.json", "w") as f:
        json.dump(meta, f, indent=2)

    # tasks.json - work items
    tasks = [w.to_dict() for w in quest.work_items]
    with open(quest_dir / "tasks.json", "w") as f:
        json.dump(tasks, f, indent=2)
