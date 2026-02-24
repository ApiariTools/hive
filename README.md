# hive

Orchestration brain for the Apiari toolchain. Plans quests, dispatches tasks to swarm agents, aggregates signals, and provides a monitoring dashboard.

## Usage

```bash
hive init                   # Initialize workspace
hive chat                   # Interactive coordinator chat
hive plan "Add auth"        # Plan a quest
hive start [id]             # Dispatch quest tasks to swarm agents
hive daemon start           # Start Telegram bot daemon
hive buzz --once            # Single signal poll
hive dashboard              # Launch TUI dashboard
```

## Configuration

Config lives in `.hive/`:
- `workspace.yaml` — workspace settings and repos
- `daemon.toml` — Telegram bot and notification settings

## Development

```bash
cargo build -p hive         # Build
cargo test -p hive          # Run tests
```
