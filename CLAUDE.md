# Hive

Workspace chat hub — Rust daemon + React SPA.

## Rules
1. You are working in a git worktree on a `swarm/*` branch. Never commit to `main`.
2. Only modify files within this repository.
3. Do NOT run `cargo install` or modify system state.
4. Run ALL checks before committing: `cargo fmt && cargo clippy -- -D warnings -A clippy::too_many_arguments && cargo test`
5. For frontend changes, also run: `cd web && npx tsc --noEmit && npx vitest run`

## Architecture

```
src/
  main.rs          — CLI + daemon startup
  routes.rs        — All HTTP/WS endpoints (axum)
  db.rs            — SQLite (conversations, sessions, bot_status, unread)
  bot.rs           — BotRunner trait + MockBotRunner for testing
  events.rs        — WebSocket event hub (broadcast)
  watcher.rs       — Specialty bot signal watchers
  config_watcher.rs — Auto-detect config/prompt changes
  lib.rs           — Public API for tests

web/
  src/App.tsx           — Main app, routing, state management
  src/api.ts            — All API calls
  src/types.ts          — TypeScript types
  src/components/
    ChatPanel.tsx       — Chat messages, input, streaming, attachments
    BotNav.tsx          — Left sidebar: bot list + unread badges
    ReposPanel.tsx      — Right sidebar: repos + workers
    TopBar.tsx          — Workspace tabs + hamburger
    WorkerDetail.tsx    — Worker info + conversation
    CommandPalette.tsx  — Cmd+K palette
```

## Design System

Dark theme. CSS variables in `web/src/theme.css`:
- `--bg: #111` `--bg-card: #191919` `--border: #282828`
- `--text: #aaa` `--text-strong: #eee` `--text-faint: #555`
- `--accent: #f5c542` (gold) `--red: #e85555` `--green: #5cb85c`
- Font: system-ui, 15px base, 16px for inputs (prevents iOS zoom)
- Icons: `lucide-react` — DO NOT use emoji icons

## CSS Rules — READ CAREFULLY
- NEVER put `overflow: hidden` on `.msg` or `.messages` — it hides content
- `overflow-x: auto` goes on individual elements (`pre`, `table`) not containers
- Use CSS modules (`.module.css`), not global CSS
- Mobile breakpoint: `768px`
- Test on mobile — iOS Safari has quirks

## Testing
- 171 tests total (123 Rust + 48 frontend)
- Rust: `cargo test` — DB, API endpoints, streaming pipeline, bot runner
- Frontend: `cd web && npx vitest run` — components + integration
- CI runs on every push/PR: fmt, clippy, tests, tsc, vitest, vite build
- MockBotRunner in `src/bot.rs` for testing bot pipeline without live CLIs
- **Add tests for any new feature or bug fix**

## Key Patterns
- Frontend is dumb — all state lives in daemon/DB
- Bot sessions run in background tasks (fire-and-forget)
- Frontend polls bot_status every 3s, gets conversations on load + WebSocket events
- `useKeyboardHeight` was removed — don't re-add iOS keyboard hacks
- Uncontrolled textarea for chat input (no React state for input value)
- `onMouseDown preventDefault` on send button keeps iOS keyboard open
- `enterKeyHint="send"` on textarea for mobile keyboard Send key

## Config
- Workspace configs: `~/.config/hive/workspaces/{name}.toml`
- DB: `~/.config/hive/hive.db` — NEVER delete this
- Bot personality: `.apiari/soul.md` in workspace root
- Project context: `.apiari/context.md` in workspace root
- Custom bot prompt: `prompt_file` field in workspace TOML

## Common Pitfalls
- `overflow: hidden` on message containers HIDES ALL TEXT — we've hit this 3 times
- Swarm state uses `worktrees` not `workers`, `agent_kind` not `agent`
- Bot responses start with `\n\n` — always trim before storing
- `GH_TOKEN` from Claude Code sandbox breaks git — daemon strips it on startup
- Canvas elements need explicit `width` — `left`/`right` doesn't stretch them
- Don't add `position: fixed` to `#root` — breaks iOS textarea focus
- The `node_modules/.vite` cache can cause phantom build errors — delete it
