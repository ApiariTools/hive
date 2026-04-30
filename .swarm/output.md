## Follow-up Scheduler Tool

Added a complete follow-up scheduling system that lets bots schedule future check-ins that fire automatically.

### What was built

**Backend:** New `followup` module with SQLite persistence, delay parsing (`30s`/`5m`/`1h`/`2h30m`), and a `FollowupWatcher` in the tick engine that checks for due follow-ups every ~15s. When a follow-up fires, its action is injected as a `[Scheduled follow-up]` user message into the bot's conversation. Includes `hive followup schedule/list/cancel` CLI subcommands, REST API endpoints (`GET /followups`, `DELETE /followups/{id}`), WebSocket events, and a system prompt section telling bots how to use the command.

**Frontend:** `FollowupCard` component with live countdown timer, cancel button, and fired/cancelled states. Sticky indicator at the bottom of chat when the card is scrolled out of view. State management in App.tsx with polling and WebSocket event handling.

### Outcome
- All 441 Rust tests pass (11 new for followup module)
- All 126 frontend tests pass (7 new for FollowupCard)
- PR: https://github.com/ApiariTools/hive/pull/63
