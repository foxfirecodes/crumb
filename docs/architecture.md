# Crumb Technical Architecture

## Repository Map

```text
React popover UI
  src/App.tsx
  src/components/*
  src/lib/ipc.ts
  src/lib/types.ts
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
3. `runtime.rs` inserts or updates a running source row, fetches messages, filters out messages inside the previously scraped range, loads existing source actions/decisions, then calls `ai::extract`.
4. `ai.rs` builds a transcript plus known Discord people, embeds, components, and existing items. The extractor returns decisions and action item candidates.
5. `db.rs::mark_extracted` stores source-local decisions/action candidates, upserts canonical action items, attaches evidence, dedupes similar canonical actions, updates the scraped message range, and emits UI updates.
6. React refreshes Actions/Sources through Tauri commands and events.

## Watching Flow

1. User runs `/watch` or `/unwatch` in Discord.
2. `discord.rs` enqueues the command. `/watch` seeds `watched_channels.last_seen_message_id` from the newest message at that moment, so the first poll does not backfill older messages.
3. `runtime.rs` has a five-minute scheduler that enqueues one poll task per watched channel.
4. A single worker consumes scrape/watch/poll tasks sequentially to reduce Discord API pressure.
5. Watch polls fetch at most the latest 100 messages and process only messages whose snowflake is newer than the watch cursor.
6. Successful extraction updates the watch cursor and sends a system notification when new canonical action items were created.

## Data Model Notes

- `action_items` are source-local extracted rows for a scrape/source detail.
- `canonical_action_items` are the main task inbox rows.
- `action_item_evidence` links canonical actions back to source evidence.
- `source_kind` is currently mostly `discord`.
- `source_scope` for Discord is the channel ID.
- Source rows are keyed like `discord:{channel_id}` for the current Discord channel MVP.
- `watched_channels` stores persistent Discord watch state and its new-message cursor.
- `status` values are `inbox | active | snoozed | done | archived`.
- The main Actions view lists `inbox` and `active`; dismissed view lists `done` and `archived`.
- `assignee` is display text. `assignee_key` is the stable filter key.
- Discord user assignee keys use `discord:user:{id}` when possible. Unknown people fall back to `person:{slug}`; groups can be `team:{slug}`.
- Action items can carry a `url`; PR review notifications use this for a direct pull request link.
- `scrapes.first_message_id` and `scrapes.last_message_id` store the fetched Discord message range. Repeated scrapes fetch the requested recent window and process only messages outside the stored range.
- The frontend persists the selected assignee/person filter in `localStorage` under `crumb.personFilter`.
- Source deletion intentionally deletes the source's canonical action items/evidence too, so re-scraping can be tested from scratch.

## AI Extraction Notes

- The system prompt in `src-tauri/src/ai.rs` is the contract for extracted JSON.
- `ExtractedActionItem.due` accepts `due`, `target_date`, and `targetDate`.
- The extractor is told to preserve relative due values like `today`, `this week`, or `next Friday`.
- Existing canonical actions are sent to the extractor so it can return `merge_with`.
- The signed-in Discord user from `/users/@me` is passed as `current_user_json`; known Discord people are derived from message authors and mentions and passed as `known_people_json`.
- Discord embeds and components are summarized into transcript text so notification-style messages with empty `content` can still produce action items.
- PR URLs are requested from the extractor and also recovered deterministically from message/embed/component text when the extractor omits them. PR-linked action items are assigned to the signed-in user.
- Approval notifications get a deterministic merge fallback: if a PR has an approval and no later merge-success notification in the scraped messages, Crumb creates a merge action item.
- ACP/Claude sessions are configured to disable tools and keep extraction constrained to JSON output.

## UI Notes

- `src/App.tsx` owns top-level view state, action status/person filters, source selection, and source deletion.
- `ActionList` renders Open/Dismissed tabs, assignee select, dismiss/restore, expandable rows, source navigation, and assignee editing.
- `ScrapeList` renders source rows and inline delete confirmation.
- `ScrapeDetail` renders source summary, decisions, source-local action candidates, and delete source.
- Delete confirmation is inline (`Delete` -> `Confirm`) instead of `window.confirm()`, because native confirm dialogs can be unreliable in the menu bar popover.

## Implementation Notes

- Keep DB migrations append-only. Use `ensure_column` in migration prepare hooks for legacy DB compatibility when adding columns.
- Preserve action item status when merging/upserting unless explicitly changing lifecycle state.
- Use source-scoped dedupe first. Be conservative with broader dedupe.
- When changing payload structs in Rust, update `src/lib/types.ts` and row mapping indexes together.
- When adding a Tauri command, update `commands.rs`, `lib.rs` invoke handler registration, and `src/lib/ipc.ts`.
