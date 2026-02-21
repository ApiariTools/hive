"""Reminder data model and operations.

Reminders are stored as JSON in .hive/reminders.json.
They integrate with the heartbeat cycle via fire_reminders(),
which converts due reminders into Signal objects.
"""

from __future__ import annotations

import json
import re
import secrets
from dataclasses import asdict, dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path

from hive.models.quest import Signal

REMINDERS_FILE = "reminders.json"
MAX_FIRED_KEPT = 100


@dataclass
class Reminder:
    id: str
    message: str
    created_at: str  # ISO 8601 UTC
    due_at: str  # ISO 8601 UTC
    fired: bool = False
    source: str = "cli"  # "cli" or "chat"

    def to_dict(self) -> dict:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict) -> Reminder:
        return cls(
            id=data["id"],
            message=data["message"],
            created_at=data["created_at"],
            due_at=data["due_at"],
            fired=data.get("fired", False),
            source=data.get("source", "cli"),
        )


def _make_id() -> str:
    return "rem_" + secrets.token_hex(4)


def _now_utc() -> datetime:
    return datetime.now(timezone.utc)


def _to_iso(dt: datetime) -> str:
    return dt.isoformat().replace("+00:00", "Z")


def parse_relative_time(spec: str) -> datetime:
    """Parse relative time spec into absolute UTC datetime.

    Supports: 5m, 1h, 2d, 30s, 1h30m, tomorrow
    """
    spec = spec.strip().lower()

    if spec == "tomorrow":
        now = _now_utc()
        tomorrow = now + timedelta(days=1)
        return tomorrow.replace(hour=9, minute=0, second=0, microsecond=0)

    # Parse compound specs like "1h30m" or simple "5m"
    pattern = re.compile(r"(\d+)\s*([smhd])")
    matches = pattern.findall(spec)
    if not matches:
        raise ValueError(f"Cannot parse time spec: {spec!r}. Use e.g. 5m, 1h, 2d, 1h30m, tomorrow")

    delta = timedelta()
    for value_str, unit in matches:
        value = int(value_str)
        if unit == "s":
            delta += timedelta(seconds=value)
        elif unit == "m":
            delta += timedelta(minutes=value)
        elif unit == "h":
            delta += timedelta(hours=value)
        elif unit == "d":
            delta += timedelta(days=value)

    return _now_utc() + delta


def load_reminders(hive_dir: Path) -> list[Reminder]:
    """Load reminders from .hive/reminders.json."""
    path = hive_dir / REMINDERS_FILE
    if not path.exists():
        return []
    try:
        with open(path) as f:
            data = json.load(f)
        return [Reminder.from_dict(r) for r in data]
    except (json.JSONDecodeError, OSError, KeyError):
        return []


def save_reminders(hive_dir: Path, reminders: list[Reminder]) -> None:
    """Save reminders to .hive/reminders.json."""
    hive_dir.mkdir(parents=True, exist_ok=True)
    path = hive_dir / REMINDERS_FILE
    with open(path, "w") as f:
        json.dump([r.to_dict() for r in reminders], f, indent=2)


def add_reminder(
    hive_dir: Path, message: str, due_at: datetime, source: str = "cli"
) -> Reminder:
    """Create and persist a new reminder."""
    reminders = load_reminders(hive_dir)
    reminder = Reminder(
        id=_make_id(),
        message=message,
        created_at=_to_iso(_now_utc()),
        due_at=_to_iso(due_at),
        source=source,
    )
    reminders.append(reminder)
    save_reminders(hive_dir, reminders)
    return reminder


def check_due_reminders(hive_dir: Path) -> list[Reminder]:
    """Return unfired reminders that are past due."""
    reminders = load_reminders(hive_dir)
    now = _now_utc()
    due = []
    for r in reminders:
        if r.fired:
            continue
        due_dt = datetime.fromisoformat(r.due_at.replace("Z", "+00:00"))
        if due_dt <= now:
            due.append(r)
    return due


def fire_reminders(hive_dir: Path) -> list[Signal]:
    """Find due reminders, convert to Signals, mark fired, save.

    Returns Signal objects for the heartbeat pipeline.
    """
    reminders = load_reminders(hive_dir)
    now = _now_utc()
    signals = []

    for r in reminders:
        if r.fired:
            continue
        due_dt = datetime.fromisoformat(r.due_at.replace("Z", "+00:00"))
        if due_dt <= now:
            r.fired = True
            signal = Signal(
                id=Signal.make_fingerprint("reminder", r.id),
                source="reminder",
                type="reminder",
                severity="high",
                title=f"Reminder: {r.message}",
                timestamp=_to_iso(now),
            )
            signals.append(signal)

    if signals:
        # Prune old fired reminders if list is getting long
        _prune_fired(reminders)
        save_reminders(hive_dir, reminders)

    return signals


def _prune_fired(reminders: list[Reminder]) -> None:
    """Remove oldest fired reminders if count exceeds MAX_FIRED_KEPT."""
    fired = [r for r in reminders if r.fired]
    if len(fired) <= MAX_FIRED_KEPT:
        return
    # Sort fired by created_at, remove oldest
    fired.sort(key=lambda r: r.created_at)
    to_remove = {r.id for r in fired[: len(fired) - MAX_FIRED_KEPT]}
    reminders[:] = [r for r in reminders if r.id not in to_remove]


def pending_reminders(hive_dir: Path) -> list[Reminder]:
    """Return all unfired reminders, sorted by due_at."""
    reminders = load_reminders(hive_dir)
    pending = [r for r in reminders if not r.fired]
    pending.sort(key=lambda r: r.due_at)
    return pending
