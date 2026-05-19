// Sidecar entrypoint. Coordinates the host bridge, Discord bot, scraper,
// and extraction. Owns no UI or persistence — those are the shell's job.

import { Bot, type ScrapeRequest } from "./discord/bot";
import { Scraper } from "./discord/scraper";
import { extract } from "./ai/extract";
import { Host } from "./host";
import type { HostRequest, InitPayload } from "./types";

const host = new Host();

// discord.js-selfbot-v13 has known crashes in its WebSocket teardown path
// when auth fails. Catch those so a bad user token doesn't kill the whole
// sidecar (and with it, the bot, which is independent).
process.on("uncaughtException", (e) => {
  host.log("error", `uncaughtException: ${(e as Error).message}`);
});
process.on("unhandledRejection", (e) => {
  host.log("error", `unhandledRejection: ${e instanceof Error ? e.message : String(e)}`);
});

let bot: Bot | null = null;
let scraper: Scraper | null = null;
let initialized = false;

host.onRequest(async (req: HostRequest) => {
  switch (req.kind) {
    case "init":
      return handleInit(req.payload);
    case "scrape":
      return handleManualScrape(req.payload.channelId, req.payload.limit);
    case "shutdown":
      return handleShutdown();
  }
});

async function handleInit(payload: InitPayload) {
  if (initialized) throw new Error("already initialized");
  if (!payload.botToken || !payload.appId) {
    throw new Error("missing DISCORD_BOT_TOKEN or DISCORD_APP_ID");
  }

  // Bot is required — without it, /scrape can't be served.
  bot = new Bot({
    host,
    appId: payload.appId,
    token: payload.botToken,
    onScrape: runScrape,
  });
  await bot.start();

  // Scraper is best-effort. If the user token is bad or login times out we
  // still come online — /scrape will reply with a clear "scraper offline"
  // message until the user fixes their .env.
  if (payload.userToken) {
    const candidate = new Scraper({ host, token: payload.userToken });
    try {
      await candidate.start();
      scraper = candidate;
    } catch (e) {
      host.log(
        "warn",
        `scraper offline: ${(e as Error).message}. /scrape will reject until DISCORD_USER_TOKEN is valid.`,
      );
      // Don't keep a half-dead client around — let the lib's internal
      // teardown run, our uncaughtException guard swallows the NPE crash.
      try {
        await candidate.shutdown();
      } catch {
        // already torn down or in a bad state — ignore
      }
    }
  } else {
    host.log("warn", "no DISCORD_USER_TOKEN provided; /scrape will be rejected");
  }

  initialized = true;
  host.emit({
    kind: "ready",
    botUser: bot.user(),
    selfUser: scraper?.user() ?? null,
  });

  return { botUser: bot.user(), selfUser: scraper?.user() ?? null };
}

async function handleShutdown() {
  await Promise.allSettled([bot?.shutdown(), scraper?.shutdown()]);
  setTimeout(() => process.exit(0), 50);
  return { ok: true };
}

// /scrape entrypoint from Discord.
function runScrape(req: ScrapeRequest) {
  // intentionally not awaited — the slash command handler returns immediately,
  // we drive the work to completion in the background.
  void doScrape(req);
}

// Manual scrape from the menubar (future). Same flow without a Discord reply.
async function handleManualScrape(channelId: string, limit: number) {
  if (!scraper || !bot) throw new Error("not initialized");
  const scrapeId = crypto.randomUUID();
  void doScrape({
    scrapeId,
    channelId,
    channelName: null,
    guildId: null,
    guildName: null,
    triggeredBy: "menubar",
    limit,
    reply: async () => {},
  });
  return { scrapeId };
}

async function doScrape(req: ScrapeRequest) {
  if (!scraper) {
    const msg =
      "Scraper is offline — your DISCORD_USER_TOKEN is missing or rejected. Re-extract it from the Discord web app (docs/discord-setup.md) and restart Crumb.";
    host.emit({ kind: "scrape.failed", scrapeId: req.scrapeId, error: msg });
    try {
      await req.reply(msg);
    } catch {
      // interaction may have expired
    }
    return;
  }

  host.emit({
    kind: "scrape.started",
    scrapeId: req.scrapeId,
    channelId: req.channelId,
    channelName: req.channelName,
    guildId: req.guildId,
    guildName: req.guildName,
    triggeredBy: req.triggeredBy,
  });

  try {
    const messages = await scraper.fetchChannelMessages(
      req.channelId,
      req.limit,
      (fetched) =>
        host.emit({ kind: "scrape.progress", scrapeId: req.scrapeId, fetched }),
    );

    await req.reply(
      `Scraped ${messages.length} message${messages.length === 1 ? "" : "s"}. Extracting…`,
    );

    const { summary, decisions, actionItems } = await extract(messages);

    host.emit({
      kind: "scrape.extracted",
      scrapeId: req.scrapeId,
      messageCount: messages.length,
      summary,
      decisions,
      actionItems,
    });

    await req.reply(
      `Done — ${messages.length} messages, ${decisions.length} decision${decisions.length === 1 ? "" : "s"}, ${actionItems.length} action item${actionItems.length === 1 ? "" : "s"}. Open Crumb to view.`,
    );
  } catch (e) {
    const msg = (e as Error).message;
    host.log("error", `scrape failed: ${msg}`);
    host.emit({ kind: "scrape.failed", scrapeId: req.scrapeId, error: msg });
    try {
      await req.reply(`Scrape failed: ${msg}`);
    } catch {
      // swallow — the original interaction may already be gone
    }
  }
}

host.start();
host.log("info", "sidecar booted, awaiting init");
