# hive

Orchestration brain for the [Apiari](https://github.com/ApiariTools) toolchain. Plans work as quests, breaks them into tasks, and dispatches them to swarm agents.

## Install

```bash
cargo install --path .
```

## Usage

```bash
# Initialize a workspace
hive init

# Interactive chat with the coordinator
hive chat

# Plan a quest (creates tasks from a description)
hive plan "Add user authentication with OAuth2"

# Check status of all quests
hive status

# Start working on a quest (dispatches tasks to swarm)
hive start <quest-id>

# Work in a different directory
hive -C /path/to/project status
```

## Configuration

Create `workspace.yaml` or `.hive/workspace.yaml`:

```yaml
name: "my-project"
repos:
  - "owner/repo1"
  - "owner/repo2"
default_agent: "claude"
soul: |
  You are a friendly, thoughtful collaborator.
  Think pair-programming buddy, not drill sergeant.
conventions: |
  - Use Rust, test everything
  - Follow conventional commits
```

## How It Works

1. **`hive plan`** — Sends your description to Claude, which breaks it into discrete tasks. Creates a quest with task list.
2. **`hive start`** — Takes a quest, sets it to Active, and spawns a swarm worktree for each task (via `swarm create`).
3. **`hive chat`** — Interactive session with Claude that has workspace context, tools (Bash, Read, Grep, WebSearch), and can dispatch work to swarm agents.
4. **`hive status`** — Shows all quests and their task progress.

## Data Model

- **Quest**: A high-level goal with a title, description, and list of tasks. Statuses: Planning, Active, Paused, Complete.
- **Task**: A discrete unit of work within a quest. Statuses: Pending, InProgress, Done, Blocked.

## Files

```
.hive/
  workspace.yaml              # Workspace configuration
  quests/
    <quest-uuid>.json          # Individual quest files
```

## Requirements

- `claude` CLI (for coordinator chat/planning)
- `swarm` CLI (for dispatching tasks to agents)
- `gh` CLI (for GitHub integration)
