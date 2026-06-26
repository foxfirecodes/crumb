# Crumb Agent Notes

## Product Goal

Crumb is a personal macOS menu bar assistant for staying oriented across work conversations. Its main job is to turn noisy source activity into a small, usable action inbox: what needs doing, who owns it, when it is due, and where it came from.

The product direction includes Discord, Asana, Notion, and manual notes. The current implementation is focused on Discord ingestion plus a Tauri menu bar UI.

## Current Shape

- **Actions** is the primary view. It shows canonical action items, supports assignee filtering, dismiss/restore, expandable details, source navigation, and assignee edits.
- **Sources** is the secondary view. It shows Discord sources, scrape summaries, source-local decisions/action candidates, and source deletion for re-scrape testing.
- Discord `/scrape` fetches messages from a channel or DM, passes normalized content to the selected ACP extractor, and reconciles extracted candidates into canonical actions.

## High-Level Architecture

```text
React popover UI
  -> Tauri commands/events
  -> Rust runtime + SQLite
  -> Discord Gateway/REST + ACP extraction
```

The frontend is deliberately small and stateful. The Rust side owns ingestion, persistence, extraction orchestration, dedupe, and lifecycle changes.

## Where To Read More

- Product/action inbox model: `docs/action-inbox-ux.md`
- Technical architecture and data flow: `docs/architecture.md`
- Discord setup/runtime notes: `docs/discord-setup.md`
- Project plan/context: `docs/plan.md`

## Common Commands

From repo root:

```sh
npm run build
```

From `src-tauri`:

```sh
cargo test
cargo fmt
```

`cargo fmt` may need sandbox escalation in Codex sessions. `cargo test` may print a sandbox `Operation not permitted` line before continuing successfully.

## Working Preferences

- Follow the existing small-component React style and plain CSS class naming.
- Keep migrations append-only and legacy-DB tolerant.
- Keep the action inbox compact by default; put verbose details behind expansion or source views.
- Be conservative with dedupe and lifecycle changes. Discord absence is not completion.
- When adding a Tauri command, update command handler, invoke registration, and frontend IPC wrapper together.
