// discord.js bot — holds the gateway connection, registers and serves
// the /scrape user-installable slash command.

import {
  ApplicationIntegrationType,
  Client,
  Events,
  GatewayIntentBits,
  InteractionContextType,
  MessageFlags,
  REST,
  Routes,
  SlashCommandBuilder,
  type ChatInputCommandInteraction,
} from "discord.js";
import type { Host } from "../host";

const SCRAPE_COMMAND = new SlashCommandBuilder()
  .setName("scrape")
  .setDescription("Pull recent messages from this channel and extract decisions + action items.")
  .addIntegerOption((opt) =>
    opt
      .setName("limit")
      .setDescription("How many recent messages to scrape (1–1000, default 200)")
      .setMinValue(1)
      .setMaxValue(1000)
      .setRequired(false),
  )
  .setIntegrationTypes([
    ApplicationIntegrationType.UserInstall,
    ApplicationIntegrationType.GuildInstall,
  ])
  .setContexts([
    InteractionContextType.Guild,
    InteractionContextType.BotDM,
    InteractionContextType.PrivateChannel,
  ]);

export type ScrapeRequest = {
  scrapeId: string;
  channelId: string;
  channelName: string | null;
  guildId: string | null;
  guildName: string | null;
  triggeredBy: string;
  limit: number;
  reply: (text: string) => Promise<void>;
};

export class Bot {
  private client: Client;
  private host: Host;
  private onScrape: (req: ScrapeRequest) => void;
  private appId: string;
  private token: string;

  constructor(opts: {
    host: Host;
    appId: string;
    token: string;
    onScrape: (req: ScrapeRequest) => void;
  }) {
    this.host = opts.host;
    this.appId = opts.appId;
    this.token = opts.token;
    this.onScrape = opts.onScrape;
    this.client = new Client({ intents: [GatewayIntentBits.Guilds] });
  }

  async start() {
    this.client.once(Events.ClientReady, (c) => {
      this.host.log("info", `bot ready as ${c.user.tag}`);
    });

    this.client.on(Events.InteractionCreate, (i) => this.handleInteraction(i));

    await this.client.login(this.token);
    await this.registerCommands();
  }

  user(): string | null {
    return this.client.user?.tag ?? null;
  }

  async shutdown() {
    await this.client.destroy();
  }

  private async registerCommands() {
    const rest = new REST({ version: "10" }).setToken(this.token);
    try {
      await rest.put(Routes.applicationCommands(this.appId), {
        body: [SCRAPE_COMMAND.toJSON()],
      });
      this.host.log("info", "registered /scrape (global)");
    } catch (e) {
      this.host.log("error", `command registration failed: ${(e as Error).message}`);
      throw e;
    }
  }

  private async handleInteraction(
    interaction: ChatInputCommandInteraction | { isChatInputCommand?: () => boolean },
  ) {
    if (!("isChatInputCommand" in interaction)) return;
    if (!interaction.isChatInputCommand?.()) return;
    const i = interaction as ChatInputCommandInteraction;
    if (i.commandName !== "scrape") return;

    await i.deferReply({ flags: MessageFlags.Ephemeral });

    const limit = i.options.getInteger("limit") ?? 200;
    const scrapeId = crypto.randomUUID();
    const channelId = i.channelId;
    const channelName =
      "name" in (i.channel ?? {}) ? (i.channel as { name?: string }).name ?? null : null;
    const guildId = i.guildId;
    const guildName = i.guild?.name ?? null;
    const triggeredBy = i.user.tag;

    this.onScrape({
      scrapeId,
      channelId,
      channelName,
      guildId,
      guildName,
      triggeredBy,
      limit,
      reply: async (text: string) => {
        try {
          await i.editReply(text);
        } catch (e) {
          this.host.log("warn", `editReply failed: ${(e as Error).message}`);
        }
      },
    });
  }
}
