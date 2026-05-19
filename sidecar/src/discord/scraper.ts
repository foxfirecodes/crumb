// discord.js-selfbot-v13 client. Authenticates as the operator and reads
// channel history they already have access to. NEVER used to write/send.

import { Client } from "discord.js-selfbot-v13";
import type { Host } from "../host";
import type { NormalizedMessage } from "../types";

export class Scraper {
  private client: Client;
  private host: Host;
  private token: string;
  private ready = false;

  constructor(opts: { host: Host; token: string }) {
    this.host = opts.host;
    this.token = opts.token;
    this.client = new Client();
  }

  async start() {
    const LOGIN_TIMEOUT_MS = 15_000;

    return new Promise<void>((resolve, reject) => {
      let settled = false;
      const finish = (fn: () => void) => {
        if (settled) return;
        settled = true;
        fn();
      };

      const timer = setTimeout(() => {
        finish(() =>
          reject(
            new Error(
              `selfbot login timed out after ${LOGIN_TIMEOUT_MS}ms (token likely invalid)`,
            ),
          ),
        );
      }, LOGIN_TIMEOUT_MS);

      this.client.once("ready", () => {
        clearTimeout(timer);
        this.ready = true;
        this.host.log("info", `scraper ready as ${this.client.user?.tag}`);
        finish(() => resolve());
      });

      this.client.login(this.token).catch((e) => {
        clearTimeout(timer);
        finish(() =>
          reject(new Error(`selfbot login failed: ${(e as Error).message}`)),
        );
      });
    });
  }

  user(): string | null {
    return this.client.user?.tag ?? null;
  }

  async shutdown() {
    if (this.ready) await this.client.destroy();
  }

  async fetchChannelMessages(
    channelId: string,
    limit: number,
    onProgress?: (fetched: number) => void,
  ): Promise<NormalizedMessage[]> {
    if (!this.ready) throw new Error("scraper not ready");

    const channel = await this.client.channels.fetch(channelId);
    if (!channel || !("messages" in channel)) {
      throw new Error(`channel ${channelId} is not text-readable`);
    }

    const messages: NormalizedMessage[] = [];
    let before: string | undefined;
    let remaining = limit;

    while (remaining > 0) {
      const batchSize = Math.min(100, remaining);
      const batch = await (channel as unknown as {
        messages: {
          fetch: (opts: { limit: number; before?: string }) => Promise<
            Map<string, unknown> | { size: number; values: () => Iterable<unknown> }
          >;
        };
      }).messages.fetch({ limit: batchSize, before });

      const iter = (batch as { values?: () => Iterable<unknown> }).values
        ? Array.from((batch as { values: () => Iterable<unknown> }).values())
        : Array.from((batch as Map<string, unknown>).values());

      if (iter.length === 0) break;

      for (const raw of iter) {
        const m = raw as {
          id: string;
          author: { tag?: string; username?: string; id: string };
          content: string;
          createdAt: Date;
          reference?: { messageId?: string } | null;
          attachments?: Map<string, { url: string }> | { values: () => Iterable<{ url: string }> };
        };
        const attachments = m.attachments
          ? Array.from(
              (m.attachments as { values: () => Iterable<{ url: string }> }).values(),
            ).map((a) => a.url)
          : [];
        messages.push({
          id: m.id,
          author: m.author.tag ?? m.author.username ?? m.author.id,
          authorId: m.author.id,
          content: m.content ?? "",
          timestamp: m.createdAt.toISOString(),
          replyToId: m.reference?.messageId ?? null,
          attachments,
        });
      }

      before = iter[iter.length - 1] && (iter[iter.length - 1] as { id: string }).id;
      remaining -= iter.length;
      onProgress?.(messages.length);

      if (iter.length < batchSize) break;
    }

    // API returns newest-first; reverse so the model sees chronological order.
    return messages.reverse();
  }
}
