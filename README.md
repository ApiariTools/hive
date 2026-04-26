# Hive

Workspace chat hub with multi-bot, multi-provider support. A Rust daemon serves an HTTP API and React SPA, connecting you to AI bots (Claude, Codex, Gemini) that can chat, watch for signals, run on schedules, and coordinate swarm workers.

## Quick Start

### Prerequisites

- Rust 1.85+
- Node 20+
- At least one provider CLI installed (e.g. `claude`)
- Optional: `gh` CLI (for GitHub watch bots), `swarm` CLI (for worker integration)

### Build & Run

```bash
cargo build --release
cd web && npm install && npm run build && cd ..
./target/release/hive --port 4200
```

Open `http://localhost:4200` in your browser.

### Configure a Workspace

Create `~/.config/hive/workspaces/my-project.toml`:

```toml
[workspace]
root = "/path/to/my-project"
name = "my-project"
description = "My awesome project"

[[bots]]
name = "CI Watch"
color = "#e85555"
role = "Monitor CI and investigate failures"
provider = "claude"
watch = ["github"]
```

Reload the app — your workspace and bots appear automatically.

## Configuration

Workspace configs live in `~/.config/hive/workspaces/{name}.toml`. See [CLAUDE.md](CLAUDE.md) for the full schema.

Optional context files in your project root:
- `.apiari/context.md` — Project context appended to all bot prompts
- `.apiari/soul.md` — Communication style for bots

## Bots

**Passive** — Chat-only bots that respond to your messages. Every bot works this way by default.

**Watch** — Poll external sources (currently GitHub PRs with failing CI) and respond autonomously. Add `watch = ["github"]` to a bot config.

**Scheduled** — Run on an interval to generate reports. Configure with `schedule_hours` and `proactive_prompt`:

```toml
[[bots]]
name = "Weekly Review"
role = "Code quality reviewer"
schedule_hours = 168
proactive_prompt = "Review recent PRs and summarize code quality trends"
```

## Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| Cmd+K | Command palette (workspaces, bots, workers) |
| Cmd+J | Focus chat input |

## Development

### Setup hooks

```bash
git config core.hooksPath hooks
```

This configures git to use the `hooks/` directory for git hooks, including a `pre-push` hook that runs all checks (fmt, clippy, test, tsc, vitest, vite build) before allowing pushes.

```bash
# Run checks
cargo fmt && cargo clippy -- -D warnings -A clippy::too_many_arguments && cargo test

# Frontend
cd web
npm install
npm run dev        # dev server
npx tsc --noEmit   # type check
npx vitest run     # tests
```

## Architecture

Rust axum daemon serves the API and a bundled React SPA. SQLite (WAL mode) stores conversations, sessions, bot status, and unread tracking. A WebSocket endpoint pushes real-time updates to the frontend.

Bot responses stream through provider SDKs (`apiari-claude-sdk`, `apiari-codex-sdk`, `apiari-gemini-sdk`) in background tasks. Session management uses prompt hashing — if the config or context files change, sessions reset automatically.

When a `.swarm/` directory exists in the workspace root, bots gain the ability to dispatch and monitor swarm workers — autonomous coding agents that run in git worktrees.
