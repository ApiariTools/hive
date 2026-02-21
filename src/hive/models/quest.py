"""Quest, WorkItem, and Signal data models."""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import Any


@dataclass
class Signal:
    """An event indicating something happened (error, regression, spike, alert)."""

    id: str  # stable fingerprint
    source: str  # sentry / email / github
    type: str  # new_issue / regression / spike / email_alert / ci_failure
    severity: str  # low / medium / high / critical
    title: str
    url: str = ""
    timestamp: str = ""
    count: int = 1
    quest_id: str | None = None
    acknowledged: bool = False

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "source": self.source,
            "type": self.type,
            "severity": self.severity,
            "title": self.title,
            "url": self.url,
            "timestamp": self.timestamp,
            "count": self.count,
            "quest_id": self.quest_id,
            "acknowledged": self.acknowledged,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Signal:
        return cls(
            id=data["id"],
            source=data["source"],
            type=data["type"],
            severity=data["severity"],
            title=data["title"],
            url=data.get("url", ""),
            timestamp=data.get("timestamp", ""),
            count=data.get("count", 1),
            quest_id=data.get("quest_id"),
            acknowledged=data.get("acknowledged", False),
        )

    @staticmethod
    def make_fingerprint(source: str, unique_key: str) -> str:
        """Create a stable fingerprint from source + unique key."""
        raw = f"{source}:{unique_key}"
        return hashlib.sha256(raw.encode()).hexdigest()[:16]


@dataclass
class WorkItem:
    """A concrete step to complete part of a Quest."""

    id: str
    quest_id: str
    type: str  # code / docs / infra / metric / investigation
    repo: str = ""
    branch: str = ""
    status: str = "todo"  # todo / in_progress / blocked / in_review / done
    pr_url: str = ""
    depends_on: list[str] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "quest_id": self.quest_id,
            "type": self.type,
            "repo": self.repo,
            "branch": self.branch,
            "status": self.status,
            "pr_url": self.pr_url,
            "depends_on": self.depends_on,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> WorkItem:
        return cls(
            id=data["id"],
            quest_id=data["quest_id"],
            type=data["type"],
            repo=data.get("repo", ""),
            branch=data.get("branch", ""),
            status=data.get("status", "todo"),
            pr_url=data.get("pr_url", ""),
            depends_on=data.get("depends_on", []),
        )


@dataclass
class Quest:
    """A GitHub issue representing an outcome."""

    id: str  # GitHub issue number
    title: str
    goal: str = ""
    definition_of_done: list[str] = field(default_factory=list)
    repos: list[str] = field(default_factory=list)
    metrics: list[str] = field(default_factory=list)
    work_items: list[WorkItem] = field(default_factory=list)
    signals: list[Signal] = field(default_factory=list)
    updated_at: str = ""

    @property
    def status(self) -> str:
        """Compute quest status from work items and signals."""
        # If any unacknowledged signal severity >= high: at_risk
        for signal in self.signals:
            if not signal.acknowledged and signal.severity in ("high", "critical"):
                return "at_risk"

        # If any work item is blocked: blocked
        for item in self.work_items:
            if item.status == "blocked":
                return "blocked"

        # If all definition_of_done are complete: done
        if self.definition_of_done and all(
            item.status == "done" for item in self.work_items
        ):
            return "done"

        # If no work items yet: planning
        if not self.work_items:
            return "planning"

        return "in_progress"

    def to_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "title": self.title,
            "goal": self.goal,
            "definition_of_done": self.definition_of_done,
            "repos": self.repos,
            "metrics": self.metrics,
            "status": self.status,
            "work_items": [w.to_dict() for w in self.work_items],
            "signals": [s.to_dict() for s in self.signals],
            "updated_at": self.updated_at,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Quest:
        return cls(
            id=data["id"],
            title=data["title"],
            goal=data.get("goal", ""),
            definition_of_done=data.get("definition_of_done", []),
            repos=data.get("repos", []),
            metrics=data.get("metrics", []),
            work_items=[WorkItem.from_dict(w) for w in data.get("work_items", [])],
            signals=[Signal.from_dict(s) for s in data.get("signals", [])],
            updated_at=data.get("updated_at", ""),
        )

    @staticmethod
    def now_iso() -> str:
        return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")
