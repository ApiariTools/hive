"""Signal deduplication and prioritization."""

from hive.models.quest import Signal

SEVERITY_ORDER = {"critical": 0, "high": 1, "medium": 2, "low": 3}
TYPE_ORDER = {"regression": 0, "spike": 1, "new_issue": 2, "ci_failure": 3, "email_alert": 4}


def deduplicate(signals: list[Signal]) -> list[Signal]:
    """Remove duplicate signals by fingerprint, keeping the one with highest count."""
    seen: dict[str, Signal] = {}
    for signal in signals:
        existing = seen.get(signal.id)
        if existing is None or signal.count > existing.count:
            seen[signal.id] = signal
    return list(seen.values())


def prioritize(signals: list[Signal]) -> list[Signal]:
    """Sort signals by priority: severity, type, quest-attached, newest first.

    Uses two-pass stable sort: first by timestamp descending, then by priority key.
    """

    def priority_key(signal: Signal) -> tuple:
        return (
            SEVERITY_ORDER.get(signal.severity, 99),
            TYPE_ORDER.get(signal.type, 99),
            0 if signal.quest_id else 1,
        )

    by_time = sorted(signals, key=lambda s: s.timestamp or "", reverse=True)
    return sorted(by_time, key=priority_key)
