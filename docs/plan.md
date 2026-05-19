# Crumb MVP — Build Plan

This is the source of truth for what we're building in the first cut. The README is the vision; this is the contract.

## MVP Scope

One shippable slice, end to end:

1. macOS menubar app (Tauri) starts on login and runs in the background.
2. The same process spawns a long-lived sidecar that holds a persistent Discord gateway connection (bot identity) and exposes a `/scrape` slash command via a **user-installable** Discord application.
3. When `/scrape` fires in any channel, the sidecar uses the operator's **personal Discord user token** (selfbot mode, separate connection) to fetch up to 200 recent messages from that channel.
4. The scraped transcript is passed to the **Claude Agent SDK** which returns a structured JSON object: `{ summary, decisions[], action_items[] }`.
5. Results are persisted to SQLite and pushed to the menubar UI over Tauri events; the user sees them in a list when they click the tray icon.

**Out of scope for MVP:** Notion, Asana, polling/background sync, push notifications, breadcrumb cross-referencing, multi-user. README features beyond the above are deferred.

## Architecture

```
┌────────────────────────────────────────────────────────────────┐
│ Tauri app (macOS menubar)                                      │
│                                                                │
│  ┌─────────────────┐         ┌──────────────────────────────┐  │
│  │ React + Vite UI │ ◀──IPC▶ │ Rust core                    │  │
│  │ (tray webview)  │  events │  • tray icon + menu          │  │
│  └─────────────────┘  cmds   │  • sqlite (tauri-plugin-sql) │  │
│                              │  • sidecar lifecycle         │  │
│                              │  • IPC bridge ↔ sidecar      │  │
│                              └──────────────┬───────────────┘  │
│                                             │ stdio JSON-lines │
│                              ┌──────────────┴───────────────┐  │
│                              │ Bun sidecar (TypeScript)     │  │
│                              │  • discord.js (bot gateway)  │  │
│                              │  • discord.js-selfbot-v13    │  │
│                              │  • @anthropic-ai/            │  │
│                              │    claude-agent-sdk          │  │
│                              └──────────────────────────────┘  │
└────────────────────────────────────────────────────────────────┘
```

### Why this split

- **Rust shell** owns UI lifecycle, the menubar, SQLite, and the only public-facing surface (the tray). Tauri 2 has first-class `tray::TrayIconBuilder` and `tauri-plugin-sql`.
- **Bun sidecar** owns everything network-and-AI. The Claude Agent SDK only ships in TypeScript and Python; `discord.js-selfbot-v13` is JS-only. Bun gives a single fast binary with native TS, embeddable via Tauri's `externalBin` mechanism.
- The two halves communicate over **stdio with newline-delimited JSON** — no TCP, no auth surface, no port conflicts.

### Process tree

```
crumb (Tauri app, single user-visible process)
└─ crumb-sidecar (Bun binary, spawned by Rust on app start, killed on app quit)
   ├─ Discord bot connection (gateway, bot token)
   └─ Discord selfbot connection (gateway, user token)
```

The user only ever launches `crumb`. The sidecar is invisible.

## Repo Layout

```
crumb/
├── README.md
├── docs/
│   ├── plan.md                  ← this file
│   └── discord-setup.md         ← credential walkthrough
├── src-tauri/                   ← Rust shell
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   ├── src/
│   │   ├── main.rs              ← entrypoint, tray, sidecar lifecycle
│   │   ├── sidecar.rs           ← stdio bridge to Bun
│   │   ├── db.rs                ← sqlx queries
│   │   └── events.rs            ← typed Tauri event payloads
│   ├── migrations/
│   │   └── 0001_init.sql
│   └── binaries/                ← compiled sidecar lands here pre-bundle
│       └── crumb-sidecar-<triple>
├── src/                         ← React frontend
│   ├── App.tsx
│   ├── main.tsx
│   ├── components/
│   └── lib/
│       └── ipc.ts               ← invoke/listen wrappers
├── sidecar/                     ← Bun TypeScript
│   ├── package.json
│   ├── tsconfig.json
│   ├── src/
│   │   ├── index.ts             ← entrypoint, host bridge
│   │   ├── host.ts              ← stdio JSON-lines protocol
│   │   ├── discord/
│   │   │   ├── bot.ts           ← discord.js gateway + /scrape handler
│   │   │   └── scraper.ts       ← selfbot fetch logic
│   │   ├── ai/
│   │   │   └── extract.ts       ← Claude Agent SDK call w/ JSON schema
│   │   └── types.ts             ← shared message types
│   └── build.ts                 ← `bun build --compile` → src-tauri/binaries/
├── .env.example
├── package.json                 ← root: tauri scripts, frontend deps
├── vite.config.ts
└── tsconfig.json
```

## Sidecar Bundling

Bun compiles the sidecar to a single static executable per target triple:

```bash
bun build sidecar/src/index.ts --compile \
  --target=bun-darwin-arm64 \
  --outfile src-tauri/binaries/crumb-sidecar-aarch64-apple-darwin
```

Tauri's `externalBin` (in `tauri.conf.json` `bundle.externalBin`) picks the right binary per host triple automatically when bundling. We grant the sidecar permission via `shell:allow-execute` with `sidecar: true` in the plugin capability config.

## IPC Contract

### Rust ↔ Sidecar (stdio, newline-delimited JSON)

Every message is `{"id": string, "kind": string, ...}`. Requests get a matching response with the same `id`. Events are unsolicited from sidecar → Rust and have no `id`.

**Rust → Sidecar:**

| kind          | payload                                  | response                              |
|---------------|------------------------------------------|---------------------------------------|
| `init`        | `{ botToken, appId, userToken }`         | `{ ok: true }` or `{ error }`         |
| `scrape`      | `{ channelId, limit }`                   | `{ scrapeId }` (work continues async) |
| `shutdown`    | `{}`                                     | sidecar exits                         |

**Sidecar → Rust (events):**

| kind                | payload                                                                  |
|---------------------|--------------------------------------------------------------------------|
| `ready`             | `{ botUser, selfUser }`                                                  |
| `scrape.started`    | `{ scrapeId, channelId, channelName, guildName }`                        |
| `scrape.progress`   | `{ scrapeId, fetched }`                                                  |
| `scrape.extracted`  | `{ scrapeId, summary, decisions: Decision[], actionItems: ActionItem[] }`|
| `scrape.failed`     | `{ scrapeId, error }`                                                    |
| `log`               | `{ level, msg }`                                                         |

### Rust ↔ Frontend (Tauri commands + events)

**Commands (frontend → Rust):**
- `list_scrapes() -> Scrape[]`
- `get_scrape(scrapeId) -> ScrapeDetail`
- `trigger_scrape(channelId, limit) -> scrapeId` (manual trigger from UI; v2)
- `open_settings()` — show creds window

**Events (Rust → frontend):**
- `scrape:new` — fires when a fresh extraction lands; UI prepends to list
- `scrape:updated` — status change
- `sidecar:status` — `connected | disconnected | error`

## SQLite Schema

Database lives at `~/Library/Application Support/com.crumb.app/crumb.db`. Managed via `tauri-plugin-sql` migrations.

```sql
-- 0001_init.sql

CREATE TABLE scrapes (
  id               TEXT PRIMARY KEY,                -- uuid
  source           TEXT NOT NULL CHECK(source IN ('discord')),
  channel_id       TEXT NOT NULL,
  channel_name     TEXT,
  guild_id         TEXT,
  guild_name       TEXT,
  triggered_by     TEXT NOT NULL,                   -- discord user id who ran /scrape
  triggered_at     INTEGER NOT NULL,                -- unix ms
  status           TEXT NOT NULL,                   -- 'running'|'extracted'|'failed'
  message_count    INTEGER,
  summary          TEXT,
  error            TEXT
);

CREATE TABLE decisions (
  id           TEXT PRIMARY KEY,
  scrape_id    TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
  text         TEXT NOT NULL,
  context      TEXT,                                -- short quoted snippet
  message_ids  TEXT,                                -- JSON array of supporting msg ids
  created_at   INTEGER NOT NULL
);

CREATE TABLE action_items (
  id           TEXT PRIMARY KEY,
  scrape_id    TEXT NOT NULL REFERENCES scrapes(id) ON DELETE CASCADE,
  text         TEXT NOT NULL,
  assignee     TEXT,                                -- discord display name if mentioned
  due          TEXT,                                -- free-text date if mentioned
  message_ids  TEXT,                                -- JSON array
  created_at   INTEGER NOT NULL
);

CREATE INDEX idx_scrapes_triggered_at ON scrapes(triggered_at DESC);
CREATE INDEX idx_decisions_scrape ON decisions(scrape_id);
CREATE INDEX idx_actions_scrape ON action_items(scrape_id);
```

We do **not** persist the raw messages in MVP — they're transient input to the LLM. (We can add a `messages` table later when we want re-extraction or breadcrumb linking.)

## /scrape Flow

```
User in Discord types /scrape (optionally /scrape limit:500)
        │
        ▼
Discord delivers interaction to bot gateway (sidecar)
        │
        ▼
sidecar/discord/bot.ts:
  • interaction.deferReply({ ephemeral: true })   ← buys 15 min
  • emit scrape.started event to Rust
        │
        ▼
sidecar/discord/scraper.ts (selfbot client):
  • channel.messages.fetch({ limit: 100, before: cursor }) × N
  • normalize to { id, author, content, timestamp, replyTo, attachments }
        │
        ▼
sidecar/ai/extract.ts:
  • query() from claude-agent-sdk
  • systemPrompt: extraction specialist
  • outputFormat: { type: 'json_schema', schema: ExtractionSchema }
  • prompt: serialized transcript
  • returns { summary, decisions[], action_items[] }
        │
        ▼
emit scrape.extracted event to Rust
        │
        ▼
Rust persists rows in SQLite, emits scrape:new to frontend
        │
        ▼
Frontend prepends item to list. Tray badge increments.
        │
        ▼
Sidecar calls interaction.editReply with a short confirmation
("Scraped 187 messages — 3 decisions, 5 action items. Open Crumb to view.")
```

### Claude extraction schema

```ts
const ExtractionSchema = {
  type: 'object',
  required: ['summary', 'decisions', 'action_items'],
  properties: {
    summary: { type: 'string', description: 'Two-sentence summary of the conversation.' },
    decisions: {
      type: 'array',
      items: {
        type: 'object',
        required: ['text'],
        properties: {
          text: { type: 'string' },
          context: { type: 'string', description: 'Quoted snippet that establishes the decision' },
          message_ids: { type: 'array', items: { type: 'string' } }
        }
      }
    },
    action_items: {
      type: 'array',
      items: {
        type: 'object',
        required: ['text'],
        properties: {
          text: { type: 'string' },
          assignee: { type: 'string', description: 'Display name of the person, or empty' },
          due: { type: 'string', description: 'Free-text date phrase, or empty' },
          message_ids: { type: 'array', items: { type: 'string' } }
        }
      }
    }
  }
}
```

We use `outputFormat: { type: 'json_schema', schema }` on the Agent SDK `query()` call and read `message.structured_output` from the result message. No tools are needed — this is a pure extraction task — so we pass `allowedTools: []` to keep latency and cost down.

## Discord Application Setup (summary)

Two separate identities, both required:

1. **Bot identity** (registered application). Holds the `/scrape` slash command. Connects via standard gateway. Must be configured as a **user-installable** app (`integration_types: [USER_INSTALL]`) so you can install it to your account once and run `/scrape` in any channel you can see — including DMs and servers where you're a member but can't add bots. Contexts: `[GUILD, BOT_DM, PRIVATE_CHANNEL]`.
2. **Your personal user account**. Provides the scraper token. We never expose this to Discord's API as a bot — it logs in as you via `discord.js-selfbot-v13`. **This violates Discord ToS** in the general case but is the only way to read channels you have access to as a human without re-architecting around the bot's own permissions. Personal-use risk is yours to accept.

Full step-by-step in [`docs/discord-setup.md`](./discord-setup.md).

## Configuration

`.env` (sidecar reads at startup; Rust passes through `init` message — never written to disk by us):

```
DISCORD_BOT_TOKEN=…
DISCORD_APP_ID=…
DISCORD_USER_TOKEN=…
```

**Anthropic auth** is *not* an env var. The Claude Agent SDK reuses the operator's existing Claude Code login (reads `~/.claude/` / OAuth session). The sidecar requires that the `claude` CLI is installed and authenticated on the host machine — which it already is, since you're talking to me through it.

Later we'll move Discord secrets into macOS Keychain via `tauri-plugin-stronghold` or `keyring-rs`. MVP just reads `.env` from the app data dir.

## Build Phases

Order matters — each phase ends with something runnable.

### Phase 0 — Scaffolding (~30 min)
- `bun create tauri-app` (React + TS template) into the existing repo.
- Add `tauri-plugin-sql` (sqlite feature).
- Set up `sidecar/` with `bun init`, install `discord.js`, `discord.js-selfbot-v13`, `@anthropic-ai/claude-agent-sdk`, `zod`.
- Wire `bun build --compile` into a root `package.json` script that runs before `tauri build`.
- Verify: `bun run tauri dev` opens a window.

### Phase 1 — Menubar (~30 min)
- Replace main window with a tray-only configuration. Use `TrayIconBuilder` with `on_tray_icon_event` to toggle a small popover window positioned near the tray icon.
- Hide from Dock (`activation_policy = ActivationPolicy::Accessory`).
- Verify: click tray icon → window shows/hides. No dock icon.

### Phase 2 — Sidecar lifecycle (~45 min)
- Add `binaries/` to `externalBin`, write a stub sidecar that prints `{"kind":"ready"}` and exits on `shutdown`.
- Rust spawns sidecar on app start, parses NDJSON from stdout, kills it on app quit.
- Surface `sidecar:status` event to the frontend; show a green/red dot in the tray popover.
- Verify: tray shows "sidecar connected" after launch.

### Phase 3 — SQLite + skeleton UI (~45 min)
- Run migration on startup.
- Implement `list_scrapes` / `get_scrape` commands returning seeded test rows.
- Frontend lists scrapes with decisions/action items expanded.
- Verify: seeded rows render correctly.

### Phase 4 — Discord bot + /scrape registration (~1 hr)
- Real sidecar: discord.js Client logs in with bot token.
- On `ready`, register the `/scrape` command globally (with `integration_types: [1]` for user-install, `contexts: [0, 1, 2]`).
- Handler: defer reply, emit `scrape.started`, mock the scraper for now (return 0 messages), emit `scrape.extracted` with hardcoded sample data.
- Rust persists, frontend updates live.
- Verify: install bot to your account, run `/scrape` in a server, see a row appear in the menubar in real time.

### Phase 5 — Real scraper (~45 min)
- discord.js-selfbot-v13 Client logs in with user token.
- Implement `fetchChannelMessages(channelId, limit)` with pagination.
- Replace the mock — `/scrape` now reads real messages.
- Surface progress events while paginating.
- Verify: scrape a real channel, message count matches Discord.

### Phase 6 — Claude extraction (~45 min)
- Wire `@anthropic-ai/claude-agent-sdk` `query()` with `outputFormat` and the schema above.
- Replace hardcoded extraction with real call.
- Handle SDK errors → emit `scrape.failed`.
- Verify: run `/scrape` on a real channel with known decisions, see them surface in the menubar.

### Phase 7 — Polish (~30 min)
- Tray badge count for unread scrapes.
- "Mark as read" / dismiss.
- Settings window for entering tokens (so we don't need `.env` for first-time setup).
- Launch-at-login.

Total estimate: ~5 hours of focused build time to working MVP.

## Risks & Mitigations

| Risk                                                                                   | Mitigation                                                                                                                                |
|----------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------------------------------------------|
| Selfbot token usage gets the user's account flagged                                    | Document the risk in `discord-setup.md`. Keep request rate low (selfbot lib already throttles). Don't auto-poll in MVP — only on demand.  |
| `discord.js-selfbot-v13` drifts behind Discord API                                     | Pin a known-working version. If it breaks, we can fall back to raw HTTP to `/api/v9/channels/.../messages` with the same token.           |
| Claude Agent SDK structured output occasionally returns malformed JSON                 | Wrap result in a Zod parse; on failure, retry once with a corrective prompt. Cap retries at 1 to bound cost.                              |
| Sidecar crashes silently                                                               | Rust restarts it up to 3 times in 60s, then surfaces a red status to the UI.                                                              |
| Tauri sidecar paths differ in dev vs bundled                                           | Single helper in Rust that resolves `crumb-sidecar` via `app.path().resource_dir()` in prod, `./src-tauri/binaries/...` in dev.           |
| Bun compile target mismatch on Intel Macs                                              | Ship both `x86_64-apple-darwin` and `aarch64-apple-darwin` binaries; Tauri picks per build. MVP: aarch64 only (operator's machine).       |

## Open Questions (low-priority, can decide during build)

These are not blockers — listed so we don't forget:

1. Should `/scrape` be installable to guilds as well as users, or strictly user-install only? (Default: user-only — keeps the threat model small.)
2. Do we want a hotkey to open the menubar window? (Default: no, MVP is mouse-driven.)
3. Should scrapes ever auto-delete? (Default: no, manual delete only.)
4. When two `/scrape` commands race on the same channel, queue or refuse? (Default: refuse with "scrape in progress".)

## What I Need From You

Per your direction, I'll build without tokens for now and produce a setup guide. To actually run the MVP end-to-end you'll later need to provide:

- `DISCORD_APP_ID` + `DISCORD_BOT_TOKEN` — created during the steps in `discord-setup.md`.
- `DISCORD_USER_TOKEN` — extracted from your browser, also covered in `discord-setup.md`.

Anthropic auth: not needed as an env var. The Claude Agent SDK piggybacks on your existing Claude Code login via the `claude` CLI on your `PATH`.

Until those land I can:
- Scaffold the full repo (phases 0–3).
- Mock the Discord + AI paths so the full UI + persistence loop is exercisable.
- Write the real Discord/AI code paths against the documented APIs and ship them disabled-but-correct, so flipping the env vars activates them.

Stop me if you'd rather wait on real tokens before scaffolding.
