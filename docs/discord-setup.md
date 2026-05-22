# Discord Setup

Crumb needs **two** Discord identities. They do different jobs and must not be confused.

| Identity        | What it is                              | Used for                                         | Where it comes from              |
|-----------------|-----------------------------------------|--------------------------------------------------|----------------------------------|
| **Bot**         | A registered Discord application        | Hosting the `/scrape` slash command              | Discord Developer Portal         |
| **You (user)**  | Your own Discord account                | Reading channel history (selfbot mode)           | Your browser session             |

Keep these tokens separate. The bot token is reissuable; your user token is your account.

---

## Risk acknowledgement: user-token scraping

Using your own user token from a script is technically a violation of [Discord's Terms of Service](https://discord.com/terms) (selfbots/automated user accounts are prohibited). The risk profile in practice:

- **What can happen:** Account flagging, temporary suspension, in extreme cases a permanent ban.
- **What lowers the risk:** Low request volume, no spamming, no rapid-fire automation. `/scrape` is on-demand only — we don't poll.
- **What this tool does:** Reads message history from channels you already have access to as a human. We never send messages, react, join servers, or do anything that mutates state from your identity.

If you're not comfortable with that risk, the alternative is to use **only** the bot identity for reading too — but the bot can only read channels you've explicitly invited it to, which defeats the purpose of "Crumb sees what I see."

By proceeding, you accept the risk for your own account.

---

## Part 1 — Create the bot application

### 1.1 Register the application

1. Go to https://discord.com/developers/applications.
2. Click **New Application**. Name it `Crumb` (or whatever you prefer — the user-facing name).
3. Note the **Application ID** on the General Information page. This is your `DISCORD_APP_ID`.

### 1.2 Configure as a user-installable app

This is the new (post–2024) Discord feature that lets a slash command run anywhere you can see, without being added to a server.

1. In your app's settings, go to **Installation**.
2. Under **Installation Contexts**, enable both:
   - ✅ **User Install**
   - ✅ **Guild Install** (optional — only if you also want to add it to specific servers)
3. Under **Install Link**, choose **Discord Provided Link**. Save the resulting URL — you'll use it to install the app to your account in step 1.5.
4. Under **Default Install Settings → User Install**:
   - **Scopes:** `applications.commands`
   - (No bot scope needed for user install — the slash command is delivered to the bot via its gateway connection, but the install itself is permission-free.)
5. Under **Default Install Settings → Guild Install** (if enabled):
   - **Scopes:** `applications.commands`, `bot`
   - **Permissions:** `Send Messages`, `Read Message History`

### 1.3 Create the bot

1. Go to the **Bot** tab in the sidebar.
2. Under **Privileged Gateway Intents**, enable:
   - No privileged intents are required for the MVP. The bot only receives slash-command interactions; message history is read separately with your user token.
3. Under **Token**, click **Reset Token** and copy the value once. This is your bot token. **Discord shows this exactly once.** Save it immediately into Crumb's Settings window.

### 1.4 Public key (informational)

On the General Information page there's also a **Public Key**. We don't need it for MVP because we use the gateway connection, not HTTP interactions. Skip.

### 1.5 Install the app to your account

1. Open the **Install Link** from step 1.2.3 in a browser where you're logged into Discord.
2. Choose **Add to My Apps** (this is the user-install option).
3. Authorize.
4. The app is now installed on your account. The `/scrape` command will appear in the command picker in any channel — DMs, group DMs, servers — once Crumb registers it (Phase 4 of the build plan).

---

## Part 2 — Extract your personal user token

> One more time: this token authenticates as **you**. Treat it like a password. Anyone holding it can impersonate you on Discord. Never paste it into a chat, a screenshot, or a public repo.

### 2.1 From the Discord web app

1. Open https://discord.com/app in a browser.
2. Open Developer Tools (`Cmd+Option+I` on macOS).
3. Go to the **Network** tab. Make sure **Preserve log** is on.
4. Refresh the page (`Cmd+R`).
5. In the Network filter box, type `science` or `users/@me`. Click any matching request.
6. In the right pane, switch to **Headers**. Scroll to **Request Headers** and find the `Authorization` header. Its value is your user token. Copy it verbatim into Crumb's Settings window.

> The Discord desktop app obfuscates this — use the web app or a real browser tab inside the desktop client's devtools.

### 2.2 Rotate when needed

If you ever change your Discord password, this token is invalidated and you'll need to re-extract it. Crumb will surface a clear error in the menubar when authentication fails.

---

## Part 3 — Populate Settings

Open **Settings...** from Crumb's tray menu and enter:

- Application ID
- Bot token
- User token

> **No `ANTHROPIC_API_KEY` needed.** Crumb is an ACP client. By default it connects to Claude Code through the pinned command `bash -ic 'npx -y @agentclientprotocol/claude-agent-acp@0.33.1'`, which reuses your existing Claude Code auth.

Optional AI settings:

- Model: `sonnet` or `haiku`
- Effort: `low`, `medium`, `high`, or `xhigh`
- Claude config dir: optional path to a separate Claude config/auth directory
- ACP command: optional alternate ACP-compatible agent command

Crumb passes Claude Code session options and environment variables to use the configured model/effort and disable project/user setting sources, hooks, tools, prompt history, and memory for extraction sessions. By default it does not override `CLAUDE_CONFIG_DIR`, so the ACP connector can reuse your normal Claude Code auth. If you set a Claude config dir, that directory must already be logged into Claude Code.

For developer builds, Crumb can import a repo-root `.env` once if no app settings file exists. Normal bundled use does not require `.env`.

---

## Verification

After Phase 4 of the build plan you should be able to:

1. Launch Crumb. The tray icon appears.
2. Click the tray icon. It shows "Bot: connected ✓ / Scraper: connected ✓".
3. Open Discord. In any channel, type `/scrape`. The autocomplete should show your Crumb command.
4. Run it. Within a few seconds: an ephemeral confirmation in Discord, and a new row in the menubar.

If autocomplete doesn't show the command:

- Wait a minute (global commands can take time to propagate the first time).
- Confirm the install succeeded by checking https://discord.com/settings/authorized-apps — Crumb should be listed under **Authorized Apps**.
- Confirm the bot is online — Crumb's logs will say `bot ready as crumb#1234`.
