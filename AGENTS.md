# Crumb Agent Notes

## What This Project Is

Crumb is a personal macOS menu bar app for tracking decisions and action items from work sources. The current implemented surface is a Tauri popover with:

- **Actions**: canonical action items extracted from sources.
- **Sources**: Discord channel scrape summaries and details.
- A Discord `/scrape` flow that fetches recent channel messages and asks an ACP/Claude extractor to identify decisions, action items, assignees, due dates, and merge/dedupe metadata.

The product direction includes Notion and Asana, but this repo is currently centered on the Tauri app plus Discord ingestion.

## High-Level Architecture

```text
React popover UI
  src/App.tsx
  src/components/*
  src/lib/ipc.ts
        |
        | Tauri invoke/events
        v
Rust Tauri backend
  src-tauri/src/commands.rs  - IPC commands
  src-tauri/src/db.rs        - SQLite access, migrations, dedupe/upsert logic
  src-tauri/src/runtime.rs   - background scrape runtime
  src-tauri/src/discord.rs   - Discord Gateway/REST scraper
  src-tauri/src/ai.rs        - ACP/Claude extraction prompt and parsing
  src-tauri/src/events.rs    - serialized payload structs sent to frontend
        |
        v
SQLite database in app data dir
  migrations in src-tauri/migrations
```

## Core Data Flow

1. User runs `/scrape` in Discord.
2. `discord.rs` receives the interaction and enqueues a `ScrapeRequest`.
3. `runtime.rs` inserts/updates a running source row, fetches messages, loads existing source actions/decisions, then calls `ai::extract`.
4. `ai.rs` builds a transcript plus known Discord people and existing items. The extractor returns decisions and action item candidates.
5. `db.rs::mark_extracted` stores source-local decisions/action candidates, upserts canonical action items, attaches evidence, dedupes similar canonical actions, and emits UI updates.
6. React refreshes Actions/Sources through Tauri commands and events.

## Important Data Model Notes

- `action_items` are source-local extracted rows for a scrape/source detail.
- `canonical_action_items` are the main task inbox rows.
- `action_item_evidence` links canonical actions back to source evidence.
- `source_kind` is currently mostly `discord`.
- `source_scope` for Discord is the channel ID.
- Source rows are keyed like `discord:{channel_id}` for the current Discord channel MVP.
- `status` values are `inbox | active | snoozed | done | archived`.
- The main Actions view lists `inbox` and `active`; dismissed view lists `done` and `archived`.
- `assignee` is display text. `assignee_key` is the stable filter key.
- Discord user assignee keys use `discord:user:{id}` when possible. Unknown people fall back to `person:{slug}`; groups can be `team:{slug}`.
- The frontend persists the selected assignee/person filter in `localStorage` under `crumb.personFilter`.
- Source deletion intentionally deletes the source's canonical action items/evidence too, so re-scraping can be tested from scratch.

## AI Extraction Notes

- The system prompt in `src-tauri/src/ai.rs` is the contract for extracted JSON.
- `ExtractedActionItem.due` accepts `due`, `target_date`, and `targetDate`.
- The extractor is told to preserve relative due values like `today`, `this week`, or `next Friday`.
- Existing canonical actions are sent to the extractor so it can return `merge_with`.
- Known Discord people are derived from message authors and mentions and passed as `known_people_json`.
- ACP/Claude sessions are configured to disable tools and keep extraction constrained to JSON output.

## UI Notes

- `src/App.tsx` owns top-level view state, action status/person filters, source selection, and source deletion.
- `ActionList` renders Open/Dismissed tabs, assignee select, dismiss, and restore.
- `ScrapeList` renders source rows and inline delete confirmation.
- `ScrapeDetail` renders source summary, decisions, source-local action candidates, and delete source.
- Delete confirmation is inline (`Delete` -> `Confirm`) instead of `window.confirm()`, because native confirm dialogs can be unreliable in the menu bar popover.

## Commands And Checks

From repo root:

```sh
npm run build
```

From `src-tauri`:

```sh
cargo test
cargo fmt
```

Notes:

- `cargo fmt` may need sandbox escalation in Codex sessions.
- `cargo test` may print a sandbox `Operation not permitted` line before continuing successfully.
- Do not assume generated `src-tauri/gen/schemas/*` needs editing for normal command changes.

## Common Files

- Frontend IPC wrappers: `src/lib/ipc.ts`
- Frontend shared types: `src/lib/types.ts`
- Main popover UI: `src/App.tsx`
- Action inbox component: `src/components/ActionList.tsx`
- Sources list/detail: `src/components/ScrapeList.tsx`, `src/components/ScrapeDetail.tsx`
- Styling: `src/styles.css`
- Tauri command registration: `src-tauri/src/lib.rs`
- Command handlers: `src-tauri/src/commands.rs`
- SQLite and migrations: `src-tauri/src/db.rs`, `src-tauri/migrations/*`
- Discord ingestion: `src-tauri/src/discord.rs`, `src-tauri/src/runtime.rs`
- Extraction prompt/schema: `src-tauri/src/ai.rs`

## Implementation Preferences

- Follow the existing simple React component style and CSS class naming.
- Keep DB migrations append-only; use `ensure_column` in migration prepare hooks for legacy DB compatibility when adding columns.
- Preserve action item status when merging/upserting unless explicitly changing lifecycle state.
- Use source-scoped dedupe first. Be conservative with broader dedupe.
- When changing payload structs in Rust, update `src/lib/types.ts` and row mapping indexes together.
- When adding a Tauri command, update `commands.rs`, `lib.rs` invoke handler registration, and `src/lib/ipc.ts`.
