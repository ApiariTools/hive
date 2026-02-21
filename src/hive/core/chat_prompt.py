"""Build system prompt for hive chat from workspace state."""

from __future__ import annotations

from hive.core.context import WorkspaceContext
from hive.core.hive_state import HiveState, load_state
from hive.core.reminders import pending_reminders
from hive.core.signals import prioritize

SOUL_MAX_CHARS = 3000
MEMORY_MAX_CHARS = 2000
CLAUDE_MD_MAX_CHARS = 6000


def build_system_prompt(context: WorkspaceContext) -> str:
    """Build system prompt from current workspace state.

    Includes soul (or fallback identity), memory, workspace info,
    active quests, signals, reminders, and CLI reference.
    """
    sections = [
        _soul_section(context),
        _memory_section(context),
        _workspace_section(context),
        _project_section(context),
        _quest_section(context),
        _signals_section(context),
        _reminders_section(context),
        _cli_reference(),
    ]
    return "\n\n".join(s for s in sections if s)


def _soul_section(context: WorkspaceContext) -> str:
    """Load .hive/soul.md if it exists, otherwise fall back to hardcoded identity."""
    soul_path = context.hive_dir / "soul.md"
    if soul_path.exists():
        try:
            content = soul_path.read_text().strip()
            if content:
                if len(content) > SOUL_MAX_CHARS:
                    content = content[:SOUL_MAX_CHARS] + "\n\n[truncated]"
                return content
        except OSError:
            pass
    return _identity_section()


def _identity_section() -> str:
    return """You are Hive, a senior engineering teammate embedded in a multi-repo workspace.

You have full access to the codebase via Claude Code tools (Read, Edit, Bash, Grep, Glob, etc.).
You can also run any hive CLI command via Bash to manage quests, check status, run heartbeats, etc.

Be direct and concise. Think like a senior engineer — focus on what matters, skip boilerplate.
When the user asks "what's going on", give them a real status update from quest state and signals.
When they ask you to do something, do it — create quests, investigate issues, start work.

## Your Role: Coordinator

You are the COORDINATOR, not a coder. You manage quests, start workers, monitor progress,
and communicate with workers on behalf of the user.

When the user wants to start work on a quest:
1. Run `hive start <quest_id>` — this creates worktrees and launches SDK workers
2. The WORKERS do the coding. You do NOT write code in worktrees yourself.
3. Use `hive worker peek <name>` to check on worker progress
4. Use `hive worker send <name> "msg"` to give workers instructions

You CAN read code to answer questions, investigate issues, or help debug.
But implementation work belongs to the workers, not you."""


def _memory_section(context: WorkspaceContext) -> str:
    """Load .hive/memory.md if it exists and has user content."""
    memory_path = context.hive_dir / "memory.md"
    if not memory_path.exists():
        return ""
    try:
        content = memory_path.read_text().strip()
    except OSError:
        return ""

    if not content:
        return ""

    if len(content) > MEMORY_MAX_CHARS:
        content = content[:MEMORY_MAX_CHARS] + "\n\n[truncated]"
    return f"# Memory (from .hive/memory.md)\n\n{content}"


def _workspace_section(context: WorkspaceContext) -> str:
    lines = [
        "# Workspace",
        f"Root: {context.root}",
    ]

    if context.has_workspace():
        try:
            workspace = context.load_workspace()
            lines.append(f"Issues repo: {workspace.defaults.issues_repo}")
            if workspace.repos:
                lines.append("Repos:")
                for key, repo in workspace.repos.items():
                    lines.append(f"  {key}: {repo.path}")
        except Exception:
            pass

    return "\n".join(lines)


def _project_section(context: WorkspaceContext) -> str:
    """Load CLAUDE.md from workspace root if it exists."""
    claude_md = context.root / "CLAUDE.md"
    if not claude_md.exists():
        return ""
    try:
        content = claude_md.read_text().strip()
    except OSError:
        return ""
    if not content:
        return ""
    if len(content) > CLAUDE_MD_MAX_CHARS:
        content = content[:CLAUDE_MD_MAX_CHARS] + "\n\n[truncated]"
    return f"# Project Context (from CLAUDE.md)\n\n{content}"


def _quest_section(context: WorkspaceContext) -> str:
    state = _load_state(context)
    if not state.quests:
        return "# Quests\nNo active quests."

    lines = ["# Active Quests"]
    for qid, quest in state.quests.items():
        lines.append(f"\n## Quest #{qid}: {quest.title}")
        lines.append(f"Status: {quest.status}")
        if quest.goal:
            lines.append(f"Goal: {quest.goal}")
        if quest.work_items:
            done = sum(1 for w in quest.work_items if w.status == "done")
            total = len(quest.work_items)
            lines.append(f"Progress: {done}/{total} work items done")
            for item in quest.work_items:
                marker = "x" if item.status == "done" else " "
                lines.append(f"  [{marker}] {item.id} ({item.type}) - {item.status}")
        open_signals = [s for s in quest.signals if not s.acknowledged]
        if open_signals:
            lines.append(f"Open signals: {len(open_signals)}")

    return "\n".join(lines)


def _signals_section(context: WorkspaceContext) -> str:
    state = _load_state(context)
    unacked = state.unacknowledged_signals()
    if not unacked:
        return "# Signals\nNo unacknowledged signals."

    sorted_signals = prioritize(unacked)
    lines = [f"# Signal Inbox ({len(sorted_signals)} unacknowledged)"]
    for signal in sorted_signals[:20]:
        quest_ref = f" (Quest #{signal.quest_id})" if signal.quest_id else ""
        lines.append(f"  [{signal.severity.upper()}] {signal.title}{quest_ref}")

    return "\n".join(lines)


def _reminders_section(context: WorkspaceContext) -> str:
    """Show pending reminders in the prompt so the agent is aware of them."""
    pending = pending_reminders(context.hive_dir)
    if not pending:
        return ""

    lines = [f"# Pending Reminders ({len(pending)})"]
    for r in pending:
        lines.append(f"  - [{r.id}] \"{r.message}\" — due {r.due_at}")
    return "\n".join(lines)


def _cli_reference() -> str:
    return """# Hive CLI Reference
Run these via the Bash tool:
  hive status              - Quest summary, signal inbox
  hive plan "goal" --repos r1,r2 - Create a new quest (ALWAYS include --repos)
  hive heartbeat --once    - Poll watchers for new signals
  hive next                - Get recommended next action
  hive issue new "title"   - Create GitHub issues
  hive start <id>           - Create worktrees + workers (--repos if quest has none)
  hive show <quest_id>     - Show quest details
  hive clean <id> --yes    - Remove worktrees
  hive remind "msg" --in X - Set a reminder (5m, 1h, 2d, tomorrow)
  hive remind list         - Show pending reminders
  hive worker list         - List active workers (tmux + SDK sessions)
  hive worker peek <name>  - See recent worker activity (from SDK history)
  hive worker send <name> "msg" - Send a message to a worker's inbox
  hive worker interrupt <name>  - Send Escape to interrupt a worker
  hive worker run <name>   - Start an SDK-managed worker (used by hive start)

IMPORTANT:
- "hive plan" is a Bash command that creates a quest. ExitPlanMode is NOT the same thing.
- ALWAYS pass --repos when creating a quest: `hive plan "goal" --repos backend,frontend`
- If a quest already exists, do NOT create a duplicate. Use `hive start <id> --repos ...` instead.
- If `hive start` fails because quest has no repos, retry with --repos flag."""


def _load_state(context: WorkspaceContext) -> HiveState:
    """Load hive state, returning empty state on error."""
    try:
        return load_state(context.hive_dir)
    except Exception:
        return HiveState()
