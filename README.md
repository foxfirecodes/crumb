# Crumb

Crumb is a tiny macOS menu bar app that turns Discord chaos into a clean action inbox.

Run `/scrape` in a channel, thread, or DM and Crumb reads the recent conversation, extracts decisions and action items, deduplicates them, and keeps them in a focused local dashboard. It is especially good at PR-notification DMs: approvals become merge reminders, failed merge queues become fix tasks, review comments become follow-ups, and successful merge messages can auto-clear stale merge tasks.

Crumb is built for people whose work happens in Discord but whose attention should not.

## What It Does

- **Finds action items in Discord**: scrape a channel, thread, or DM and get a task list instead of rereading a wall of messages.
- **Tracks decisions**: Crumb keeps source-level decision summaries alongside action items so you can remember what was agreed to and where.
- **Deduplicates follow-ups**: repeated scrapes merge related action items instead of making a fresh pile every time.
- **Keeps source context**: every action links back to the source scrape, and PR-related items can carry a direct GitHub PR link.
- **Understands assignees**: filter the inbox by person, update an action's assignee, and persist your preferred assignee filter.
- **Lets you dismiss and restore**: finish or hide items, then restore them later if needed.
- **Watches important channels**: run `/watch` and Crumb polls for new messages every few minutes, processing only messages newer than the watch cursor.
- **Sends notifications**: new action items from scrape/watch runs can trigger system notifications.

## Why It Is Nice

Discord is great for collaboration and terrible as a task system. Useful commitments are buried in threads, bot messages, PR notifications, and one-off DMs. Crumb gives you a second surface that is calmer:

- **Actions** is your working inbox. Filter by open/dismissed state or by assignee. Expand items when you need source, due date, PR link, status, and evidence count.
- **Sources** is your audit trail. See every scraped Discord source, its summary, extracted decisions, and source-local action candidates.
- **Delete a source** when you want to test a clean re-scrape; Crumb drops the source and its action items.

## PR Workflow Support

Crumb has deterministic handling for common GitHub/merge-queue notification patterns:

- PR approval with no later merge success creates a **merge this PR** action.
- Merge queue failure creates a **resolve merge queue failure** action.
- Merge queue success can auto-complete the stale merge action.
- Human review and BugBot feedback become action items assigned to you.
- PR URLs are canonicalized, so links to comments/fragments still match the PR.

Merge queue outcome detection intentionally ignores embed titles because those titles are usually just PR titles. It looks at the notification body for messages like "successfully merged", "successfuwwy mewged", or "Tests failed. The PR won't merge..."

## Discord Commands

Use these from Discord after Crumb is running:

```text
/scrape [limit]
```

Fetch recent messages from the current channel/DM/thread and extract decisions + action items. Repeated scrapes avoid reprocessing messages already inside the previously scraped range.

```text
/watch
```

Start polling this channel/DM/thread. Crumb seeds the cursor at "now", so it only processes future messages.

```text
/unwatch
```

Stop polling this source.

## Setup

Crumb currently runs as a local developer app. You will need:

- macOS
- Node/npm
- Rust + Cargo
- Tauri system prerequisites
- Claude Code installed and logged in
- A Discord application/bot token
- Your Discord user token for reading message history

> Discord user-token scraping is selfbot-adjacent and may violate Discord's Terms of Service. Crumb uses it read-only so it can see the channels/DMs you can see, but you should understand the risk before using it. See [docs/discord-setup.md](docs/discord-setup.md) for details.

1. Install dependencies:

```bash
npm install
```

2. Copy the environment template:

```bash
cp .env.example .env
```

3. Fill in `.env`:

```bash
DISCORD_APP_ID=...
DISCORD_BOT_TOKEN=...
DISCORD_USER_TOKEN=...
```

Claude auth is reused from your existing Claude Code login. You do not need an Anthropic API key for the default setup.

4. Run the app:

```bash
npm run tauri:dev
```

5. Install the Discord app to your account, then try `/scrape` in a DM or channel.

For the full Discord app/token walkthrough, read [docs/discord-setup.md](docs/discord-setup.md).

## Useful Commands

```bash
npm run tauri:dev   # run the menu bar app locally
npm run build       # typecheck and build the frontend
cargo test          # run Rust tests from src-tauri/
```

## Data And Privacy

Crumb stores its local state in a SQLite database in the app data directory. Discord messages are fetched for extraction and summarized into local sources/action evidence. Your `.env` contains sensitive credentials and must stay out of git.

## Current Scope

Crumb is a personal workflow tool, not a team SaaS. Today it focuses on Discord, PR notifications, action item extraction, and a local macOS menu bar UI.

