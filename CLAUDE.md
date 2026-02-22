# Hive

Orchestration brain — plans quests, dispatches tasks to swarm agents, aggregates signals, and provides a dashboard.

## Quick Reference

```bash
cargo build -p hive                      # Build
cargo test -p hive                       # Run tests (101 unit tests)
cargo run -p hive -- init                # Initialize workspace
cargo run -p hive -- chat                # Interactive coordinator chat
cargo run -p hive -- plan "..."          # Plan a quest
cargo run -p hive -- status              # Show quest status
cargo run -p hive -- start ID            # Dispatch quest tasks to swarm
cargo run -p hive -- daemon start        # Start Telegram bot daemon
cargo run -p hive -- buzz --once         # Single signal poll
cargo run -p hive -- buzz --daemon       # Continuous signal polling
cargo run -p hive -- dashboard           # Launch TUI dashboard
cargo run -p hive -- dashboard --once    # Print status and exit
```

## Git Workflow

- You are working in a swarm worktree on a `swarm/*` branch. Stay on this branch.
- NEVER push to or merge into `main` directly.
- When done, create a PR from your branch. Swarm will handle merging.
- NEVER run `git push origin main` or `git checkout main`.

## Architecture

```
src/
  main.rs               # CLI (clap) with subcommands
  signal.rs             # Signal/Severity types (shared across buzz and daemon)
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
  channel/
    mod.rs              # Channel trait + ChannelEvent types
    telegram.rs         # Telegram Bot API client
  daemon/
    mod.rs              # Telegram bot + buzz auto-triage event loop
    config.rs           # Daemon config (daemon.toml)
    session_store.rs    # Per-chat Claude session tracking
  buzz/
    mod.rs              # Signal aggregator entry point (run logic)
    config.rs           # BuzzConfig TOML schema + validation
    output.rs           # OutputMode (stdout, file, webhook)
    reminder.rs         # Scheduled reminder signals
    signal.rs           # deduplicate(), prioritize() functions
    watcher/
      mod.rs            # Watcher trait, state persistence, registry
      github.rs         # GitHub watcher (gh CLI)
      sentry.rs         # Sentry watcher (HTTP API)
      webhook.rs        # Webhook receiver (stub)
  keeper/
    mod.rs              # Dashboard entry point (run logic)
    discovery.rs        # Tmux session discovery, buzz signal reading, PR queries
    tui/
      mod.rs            # TUI event loop (crossterm key handling)
      app.rs            # App state (sessions, selection, panels, overlays)
      render.rs         # Ratatui rendering
      theme.rs          # Honey/amber bee color palette
```

## Key Concepts

- **Quest**: High-level goal with tasks. Persisted as `.hive/quests/<id>.json`.
- **Task**: Discrete work unit within a quest. Assigned to swarm worktrees.
- **Coordinator**: Uses `apiari-claude-sdk` to run Claude sessions for chat and planning. Falls back to offline mode if `claude` CLI is unavailable.
- **Worker**: Calls `swarm create`, `swarm status`, `swarm send`, `swarm close`, `swarm merge` as subprocesses.
- **QuestStore**: CRUD over `.hive/quests/` directory using `apiari_common::state` for atomic writes.
- **Signal**: Event from external sources (Sentry, GitHub, reminders). Defined in `signal.rs`, produced by buzz watchers, consumed by daemon for triage.
- **Watcher**: Pluggable source that polls for new signals. Trait in `buzz::watcher`.
- **Keeper**: Read-only TUI dashboard that discovers swarm tmux sessions and displays their status.

## CLI Subcommands

| Command | Description |
|---------|-------------|
| `init` | Creates `.hive/` dir with `workspace.yaml` and `quests/` |
| `status` | Lists repos and all quests with task progress |
| `chat` | Interactive Claude session with workspace context |
| `plan <desc>` | Claude generates task list, saves as quest |
| `start [id]` | Activates quest, spawns swarm worktree per task |
| `daemon start` | Start persistent Telegram bot + buzz auto-triage |
| `daemon stop` | Stop the running daemon |
| `daemon restart` | Restart the daemon |
| `buzz` | Poll signal sources (Sentry, GitHub, webhooks, reminders) |
| `dashboard` | Launch read-only TUI for monitoring swarm sessions |

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

hive buzz --daemon
  |
  v (writes)
.buzz/signals.jsonl
  |
  +---> hive daemon (reads for auto-triage)
  +---> hive dashboard (reads for display)

hive dashboard
  |
  v (reads)
.swarm/state.json + tmux queries + .buzz/signals.jsonl
```

- **Uses**: `apiari-common` (IPC, state persistence), `apiari-claude-sdk` (coordinator/planning/chat)
- **Calls**: `swarm` CLI via Claude's Bash tool (create, status, send, close, merge)
- **Reads**: buzz signals (daemon auto-triage + dashboard display)
- **GitHub**: `gh` CLI wrapper for issues/PRs

## Files on Disk

```
.hive/
  workspace.yaml              # Workspace configuration
  daemon.toml                 # Daemon configuration
  daemon.pid                  # Daemon PID file
  daemon.log                  # Daemon log output
  sessions.json               # Per-chat session state
  quests/
    <quest-uuid>.json          # Quest with embedded tasks

.buzz/
  config.toml                 # Buzz watcher configuration
  state.json                  # Watcher cursors (auto-managed)
  signals.jsonl               # Signal output (when mode = "file")
```
