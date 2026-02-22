# Hive

Orchestration brain — plans quests, dispatches tasks to swarm agents.

## Quick Reference

```bash
cargo build -p hive             # Build
cargo test -p hive              # Run tests (none yet)
cargo run -p hive -- init       # Initialize workspace
cargo run -p hive -- chat       # Interactive coordinator chat
cargo run -p hive -- plan "..." # Plan a quest
cargo run -p hive -- status     # Show quest status
cargo run -p hive -- start ID   # Dispatch quest tasks to swarm
```

## Architecture

```
src/
  main.rs               # CLI (clap) with 5 subcommands
  workspace/
    mod.rs              # Workspace config (YAML), load/init, config search
  quest/
    mod.rs              # Quest, Task, QuestStatus, TaskStatus, QuestStore
  coordinator/
    mod.rs              # Claude SDK chat loop, planning, system prompt
  worker/
    mod.rs              # Subprocess calls to swarm CLI
  github/
    mod.rs              # gh CLI wrapper (issues, PRs)
```

## Key Concepts

- **Quest**: High-level goal with tasks. Persisted as `.hive/quests/<id>.json`.
- **Task**: Discrete work unit within a quest. Assigned to swarm worktrees.
- **Coordinator**: Uses `apiari-claude-sdk` to run Claude sessions for chat and planning. Falls back to offline mode if `claude` CLI is unavailable.
- **Worker**: Calls `swarm create`, `swarm status`, `swarm send`, `swarm close`, `swarm merge` as subprocesses.
- **QuestStore**: CRUD over `.hive/quests/` directory using `apiari_common::state` for atomic writes.

## CLI Subcommands

| Command | Description |
|---------|-------------|
| `init` | Creates `.hive/` dir with `workspace.yaml` and `quests/` |
| `status` | Lists repos and all quests with task progress |
| `chat` | Interactive Claude session with workspace context |
| `plan <desc>` | Claude generates task list, saves as quest |
| `start [id]` | Activates quest, spawns swarm worktree per task |

## Workspace Config (`.hive/workspace.yaml`)

```yaml
name: "my-project"          # Workspace name [default: "apiari"]
repos:                       # Repos to manage
  - "owner/repo"
default_agent: "claude"      # "claude" or "codex"
soul: |                      # Coordinator personality
  You are a senior engineer...
conventions: |               # Coding standards
  Use Rust, test everything...
```

Config search walks upward from cwd looking for `workspace.yaml`, `workspace.yml`, `.hive/workspace.yaml`, or `.hive/workspace.yml`.

## Coordinator Details

- **System prompt**: The `soul` field from workspace.yaml is used as the coordinator's personality. If no soul is set, falls back to a minimal default. The prompt also includes workspace context, available tools, and active quest summaries.
- **Tools**: Chat sessions have Bash, Read, Glob, Grep, WebSearch, and WebFetch. The coordinator can run `swarm create`, search the codebase, and look things up on the web.
- **Planning prompt**: Instructs Claude to output task titles as `- ` prefixed bullet points.
- **Task parsing**: Handles `- `, `* `, numbered lists (`1. `), strips bold markdown (`**...**`).
- **Thinking indicator**: Shows `thinking...` on stdout while waiting for Claude's response, clears when response arrives.
- **Graceful fallback**: If `claude` CLI spawn fails (ProcessSpawn error), prints warning and enters offline mode (chat echoes, plan creates stub tasks).
- **Async stdin**: Uses `tokio::spawn_blocking` with mpsc channel to read user input without blocking the event loop.

## Integration Map

```
hive plan "Add auth"
  |
  v (Claude SDK)
claude CLI session
  |
  v (creates quest)
.hive/quests/<id>.json

hive start <id>
  |
  v (subprocess)
swarm create "task prompt"    # One per task
  |
  v
.swarm/inbox.jsonl -> swarm TUI -> agent in tmux pane
```

- **Uses**: `apiari-common` (state persistence), `apiari-claude-sdk` (coordinator/planning/chat)
- **Calls**: `swarm` CLI via Claude's Bash tool (create, status, send, close, merge)
- **Reads**: buzz signals (planned, not yet wired)
- **GitHub**: `gh` CLI wrapper for issues/PRs (available but not yet called from coordinator)

## Files on Disk

```
.hive/
  workspace.yaml              # Workspace configuration
  quests/
    <quest-uuid>.json          # Quest with embedded tasks
```
