# Crumb MVP Build Plan

This is the implementation contract for the current first cut.

## MVP Scope

1. A macOS menubar app runs as one Tauri process.
2. The Rust backend owns the Discord bot gateway connection and registers a user-installable `/scrape` command.
3. When `/scrape` runs, Rust fetches message history with the operator's Discord user token using Discord's REST API.
4. The transcript is sent through the Agent Client Protocol Rust SDK to the Claude ACP connector.
5. Results are persisted to SQLite and pushed to the menubar UI over Tauri events.

Out of scope for MVP: Notion, Asana, polling/background sync, push notifications, breadcrumb cross-referencing, multi-user.

## Architecture

```
crumb (Tauri app)
├─ React + Vite popover UI
└─ Rust backend
   ├─ tray/menu/window lifecycle
   ├─ SQLite persistence
   ├─ Discord bot gateway + slash command registration
   ├─ Discord REST message-history scraper
   └─ ACP client -> Claude ACP connector
```

There is no Bun sidecar and no JavaScript Discord worker. The frontend event names still use `sidecar:status` for compatibility with the existing UI IPC layer, but the implementation is in-process Rust.

## Rust Modules

```
src-tauri/src/
├── ai.rs          ACP extraction client
├── discord.rs     Discord gateway, interactions, and REST scraper
├── runtime.rs     app runtime orchestration
├── db.rs          SQLite persistence
├── settings.rs    app settings loading and legacy .env import
├── events.rs      Tauri event payloads
├── commands.rs    frontend invoke handlers
└── lib.rs         Tauri setup, tray, popover lifecycle
```

## Settings

Crumb stores Discord and AI settings in app data as `settings.json`. Developer builds can import a repo-root `.env` once when no app settings file exists, but bundled app usage is settings-window driven.

AI auth is handled by the Claude Code ACP connector. By default Crumb pins the connector version and spawns:

```bash
npx -y @agentclientprotocol/claude-agent-acp@0.33.1
```

The settings window can override the ACP command with any other ACP-compatible agent command. Crumb passes ACP session options and environment variables that default the model to `sonnet`, default effort to `low`, restrict model selection to the configured Sonnet/Haiku family, disable Claude Code setting sources/hooks/tools for extraction, and skip prompt history. It reuses normal Claude Code auth by default, unless a separate Claude config directory is configured.

## /scrape Flow

1. User runs `/scrape limit:N` in Discord.
2. Discord delivers an `INTERACTION_CREATE` event over the bot gateway.
3. Rust defers an ephemeral interaction reply.
4. Rust upserts a running Source row keyed by the Discord channel/thread and emits `scrape:new`.
5. Rust calls `GET /channels/{channel_id}/messages` with the user token, paginating in batches of up to 100 messages.
6. Rust formats the transcript chronologically, includes existing source actions/decisions for reconciliation, and sends it to the ACP connector.
7. The ACP response is parsed as JSON with:

```json
{
  "summary": "string",
  "decisions": [{ "text": "string", "context": "string", "message_ids": ["string"] }],
  "action_items": [{ "text": "string", "assignee": "string", "due": "string", "message_ids": ["string"] }]
}
```

8. Rust writes extracted rows to SQLite, emits `scrape:updated`, and edits the Discord reply.

## Frontend Contract

Tauri commands:

- `list_scrapes()`
- `get_scrape(id)`
- `get_sidecar_status()`
- `hide_popover()`

Tauri events:

- `scrape:new`
- `scrape:updated`
- `sidecar:status`

## Notes

- The user-token scraper is read-only and only fetches history on demand.
- User-token scraping still carries Discord account risk; see `docs/discord-setup.md`.
- ACP tool permission requests are denied by the Crumb client. The extraction prompt also instructs the connector not to use tools.
