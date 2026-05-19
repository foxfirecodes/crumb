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
  assignee: string | null;
  due: string | null;
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
  assignee: string | null;
  due: string | null;
  priority: number;
  relevanceScore: number;
  firstSeenAt: number;
  lastSeenAt: number;
  completedAt: number | null;
  snoozedUntil: number | null;
  latestContext: string | null;
  evidenceCount: number;
}

export interface ScrapeDetail {
  scrape: ScrapeSummary;
  decisions: Decision[];
  actionItems: ActionItem[];
}

export type SidecarStatus =
  | { kind: "starting" }
  | { kind: "connected"; botUser: string | null; selfUser: string | null }
  | { kind: "disconnected" }
  | { kind: "error"; message: string };
