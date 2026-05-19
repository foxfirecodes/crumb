// Claude Agent SDK call. Pure JSON-schema extraction — no tools enabled,
// no filesystem access, no internet. The SDK reuses the operator's existing
// Claude Code OAuth session via the `claude` CLI on PATH.

import { query } from "@anthropic-ai/claude-agent-sdk";
import { z } from "zod";
import type {
  ExtractedActionItem,
  ExtractedDecision,
  NormalizedMessage,
} from "../types";

const ExtractionSchema = z.object({
  summary: z.string(),
  decisions: z.array(
    z.object({
      text: z.string(),
      context: z.string().optional(),
      message_ids: z.array(z.string()).optional(),
    }),
  ),
  action_items: z.array(
    z.object({
      text: z.string(),
      assignee: z.string().optional(),
      due: z.string().optional(),
      message_ids: z.array(z.string()).optional(),
    }),
  ),
});

type Extraction = z.infer<typeof ExtractionSchema>;

const SYSTEM_PROMPT = `You are an extraction specialist. You receive a chronological transcript of Discord messages from a single channel. Your job is to identify:

1. DECISIONS — concrete choices made during the conversation. A decision is something the group settled on, not a question or proposal. Quote the supporting message snippet in "context".
2. ACTION ITEMS — concrete things someone committed to do, with assignee and due date if mentioned. If unassigned, leave assignee empty.
3. A two-sentence SUMMARY of the conversation.

Rules:
- Be conservative. Only surface things that are clearly decisions or commitments, not idle discussion.
- Quote the original wording in "context"; do not paraphrase decisions.
- "message_ids" should list the IDs of the messages that establish each item.
- If there are no decisions or action items, return empty arrays. Do not invent items.
- Return ONLY JSON matching the schema. No prose.`;

const JSON_SCHEMA = {
  type: "object",
  required: ["summary", "decisions", "action_items"],
  properties: {
    summary: { type: "string" },
    decisions: {
      type: "array",
      items: {
        type: "object",
        required: ["text"],
        properties: {
          text: { type: "string" },
          context: { type: "string" },
          message_ids: { type: "array", items: { type: "string" } },
        },
      },
    },
    action_items: {
      type: "array",
      items: {
        type: "object",
        required: ["text"],
        properties: {
          text: { type: "string" },
          assignee: { type: "string" },
          due: { type: "string" },
          message_ids: { type: "array", items: { type: "string" } },
        },
      },
    },
  },
} as const;

async function resolveClaude(): Promise<string | null> {
  // `which claude` — works on macOS/Linux. We don't care about Windows yet.
  try {
    const proc = Bun.spawn(["which", "claude"], {
      stdout: "pipe",
      stderr: "ignore",
    });
    const out = (await new Response(proc.stdout).text()).trim();
    await proc.exited;
    return out || null;
  } catch {
    return null;
  }
}

function formatTranscript(messages: NormalizedMessage[]): string {
  const lines: string[] = [];
  for (const m of messages) {
    const replyMarker = m.replyToId ? ` (reply to ${m.replyToId})` : "";
    const attachMarker =
      m.attachments.length > 0 ? ` [+${m.attachments.length} attachment(s)]` : "";
    lines.push(
      `[${m.timestamp}] [${m.id}] <${m.author}>${replyMarker}${attachMarker}: ${m.content}`,
    );
  }
  return lines.join("\n");
}

export async function extract(
  messages: NormalizedMessage[],
): Promise<{
  summary: string;
  decisions: ExtractedDecision[];
  actionItems: ExtractedActionItem[];
}> {
  if (messages.length === 0) {
    return { summary: "No messages found.", decisions: [], actionItems: [] };
  }

  const transcript = formatTranscript(messages);
  const prompt = `Analyze this Discord channel transcript and extract decisions, action items, and a summary.\n\n<transcript>\n${transcript}\n</transcript>`;

  let structured: unknown = null;

  // When this sidecar is bun-compiled, the SDK's bundled CLI is not on disk
  // as a separate file — point at the user's installed `claude` on PATH,
  // which is the same auth context they're already using interactively.
  const claudePath =
    process.env.CRUMB_CLAUDE_PATH ?? (await resolveClaude()) ?? "claude";

  for await (const message of query({
    prompt,
    options: {
      systemPrompt: SYSTEM_PROMPT,
      allowedTools: [],
      maxTurns: 1,
      outputFormat: { type: "json_schema", schema: JSON_SCHEMA },
      pathToClaudeCodeExecutable: claudePath,
    } as Parameters<typeof query>[0]["options"],
  })) {
    if (
      message.type === "result" &&
      (message as { structured_output?: unknown }).structured_output
    ) {
      structured = (message as { structured_output?: unknown }).structured_output;
      break;
    }
  }

  if (!structured) {
    throw new Error("Claude returned no structured output");
  }

  const parsed = ExtractionSchema.safeParse(structured);
  if (!parsed.success) {
    throw new Error(`extraction schema mismatch: ${parsed.error.message}`);
  }

  const data: Extraction = parsed.data;
  return {
    summary: data.summary,
    decisions: data.decisions,
    actionItems: data.action_items,
  };
}
