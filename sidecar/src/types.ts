// Wire-format types shared between the Rust shell and the sidecar.
// Anything traveling over stdio must be representable here.

export type HostRequest =
  | { id: string; kind: "init"; payload: InitPayload }
  | { id: string; kind: "scrape"; payload: ScrapePayload }
  | { id: string; kind: "shutdown"; payload: Record<string, never> };

export type HostResponse =
  | { id: string; ok: true; result?: unknown }
  | { id: string; ok: false; error: string };

export type SidecarEvent =
  | { kind: "ready"; botUser: string | null; selfUser: string | null }
  | { kind: "log"; level: "info" | "warn" | "error" | "debug"; msg: string }
  | {
      kind: "scrape.started";
      scrapeId: string;
      channelId: string;
      channelName: string | null;
      guildId: string | null;
      guildName: string | null;
      triggeredBy: string;
    }
  | { kind: "scrape.progress"; scrapeId: string; fetched: number }
  | {
      kind: "scrape.extracted";
      scrapeId: string;
      messageCount: number;
      summary: string;
      decisions: ExtractedDecision[];
      actionItems: ExtractedActionItem[];
    }
  | { kind: "scrape.failed"; scrapeId: string; error: string };

export interface InitPayload {
  botToken: string | null;
  appId: string | null;
  userToken: string | null;
}

export interface ScrapePayload {
  channelId: string;
  limit: number;
}

export interface ExtractedDecision {
  text: string;
  context?: string;
  message_ids?: string[];
}

export interface ExtractedActionItem {
  text: string;
  assignee?: string;
  due?: string;
  message_ids?: string[];
}

export interface NormalizedMessage {
  id: string;
  author: string;
  authorId: string;
  content: string;
  timestamp: string;
  replyToId: string | null;
  attachments: string[];
}
