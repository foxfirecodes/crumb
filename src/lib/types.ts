export type ScrapeStatus = "running" | "extracted" | "failed";

export interface ScrapeSummary {
  id: string;
  source: "discord";
  channelId: string;
  channelName: string | null;
  guildId: string | null;
  guildName: string | null;
  triggeredBy: string;
  triggeredAt: number;
  status: ScrapeStatus;
  messageCount: number | null;
  summary: string | null;
  error: string | null;
}

export interface Decision {
  id: string;
  scrapeId: string;
  text: string;
  context: string | null;
  messageIds: string[];
  createdAt: number;
}

export interface ActionItem {
  id: string;
  scrapeId: string;
  text: string;
  assigneeKey: string | null;
  assignee: string | null;
  due: string | null;
  url: string | null;
  messageIds: string[];
  createdAt: number;
}

export type CanonicalActionStatus =
  | "inbox"
  | "active"
  | "snoozed"
  | "done"
  | "archived";

export interface CanonicalActionItem {
  id: string;
  title: string;
  status: CanonicalActionStatus;
  sourceKind: "discord" | "asana" | "manual" | "mixed";
  sourceScope: string;
  sourceLabel: string | null;
  assigneeKey: string | null;
  assignee: string | null;
  due: string | null;
  url: string | null;
  priority: number;
  relevanceScore: number;
  firstSeenAt: number;
  lastSeenAt: number;
  completedAt: number | null;
  snoozedUntil: number | null;
  latestContext: string | null;
  evidenceCount: number;
}

export type ActionItemStatusFilter = "open" | "dismissed" | "all";

export interface ScrapeDetail {
  scrape: ScrapeSummary;
  decisions: Decision[];
  actionItems: ActionItem[];
}

export type SidecarStatus =
  | { kind: "starting" }
  | { kind: "needssetup"; missing: string[] }
  | { kind: "connected"; botUser: string | null; selfUser: string | null }
  | { kind: "disconnected" }
  | { kind: "error"; message: string };

export interface AppSettings {
  discordAppId: string;
  discordBotToken: string;
  discordUserToken: string;
  aiModel: string;
  aiEffort: string;
  claudeConfigDir: string;
  acpAgentCommand: string;
}

export interface SettingsTestResult {
  ok: boolean;
  message: string;
}
