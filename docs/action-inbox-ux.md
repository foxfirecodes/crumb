# Action Inbox UX

Crumb's primary product surface is a live action inbox. Scrapes, polls, and manual additions are ingestion paths that create or update canonical action items.

## Product Shape

The menu bar opens to **Actions**.

Actions should answer: "What do I actually need to do?"

The secondary view is **Sources**. Sources are the places Crumb learned from: Discord channels or threads, Asana tasks, future Notion pages, manual notes, and scrape/poll runs. Source detail views preserve the thread/scrape context without making that history the main workflow.

## Data Model

### Canonical Action Item

A single real-world task.

Fields:

- `id`
- `title`
- `status`: `inbox | active | snoozed | done | archived`
- `assignee`
- `due`
- `priority`
- `source_kind`: `discord | asana | manual | mixed`
- `source_scope`: stable source namespace, such as a Discord channel ID or Asana workspace/project ID
- `dedupe_key`: normalized action identity inside the source scope
- `first_seen_at`
- `last_seen_at`
- `completed_at`
- `snoozed_until`
- `relevance_score`

### Evidence

Evidence explains why Crumb believes the action exists.

Evidence can include:

- scrape or poll run ID
- Discord channel/thread/message IDs
- Asana task IDs
- quoted context
- source label
- created timestamp

### Source

A source is a navigable origin for context. Examples:

- Discord channel or thread
- Discord scrape run
- Asana task
- Asana project
- manual item

## Ingestion Flow

Every source ingestion path produces candidates:

```text
source scrape/poll/manual entry
-> extracted candidates
-> reconcile with canonical action items
-> upsert action items and evidence
-> refresh Actions view
```

Scrape history remains available under Sources.

## Dedupe

Initial dedupe is deterministic and source-scoped:

1. Build a `source_scope`.
   - Discord: channel ID for MVP.
   - Asana: task ID when available, otherwise project/workspace.
   - Manual: manual namespace.
2. Normalize action text:
   - lowercase
   - trim punctuation/noise
   - collapse whitespace
   - strip common commitment prefixes where safe
3. Upsert by `(source_kind, source_scope, dedupe_key)`.
4. If a match exists:
   - preserve status
   - update `last_seen_at`
   - fill missing assignee/due
   - append evidence
5. If no match exists, create a new canonical action item.

Future semantic dedupe can run after deterministic matching. It should compare only a small candidate set: same source/project, open statuses, recent `last_seen_at`, and similar normalized text.

## Growth Control

The app must not let the top-level list grow indefinitely.

Main list rules:

- Show only `inbox` and `active` by default.
- Hide `done`, `archived`, and snoozed-until-future items.
- Sort by relevance, due date, and recency.
- Keep the visible list compact.

Lifecycle:

- `inbox`: newly discovered or needs review.
- `active`: clearly relevant to the user.
- `snoozed`: hidden until `snoozed_until`.
- `done`: completed, hidden from the main view.
- `archived`: dismissed, hidden from the main view.

Important source policy:

- Discord absence is not completion. A later scrape window may simply not include the older message.
- Asana completion can be authoritative once Asana polling exists.

## Menu Bar Navigation

Top level:

- **Actions**: canonical open action items.
- **Sources**: Discord scrapes/channels/threads and future Asana/manual sources.

Action detail should show:

- title
- status controls: done, snooze, archive
- assignee/due
- latest evidence
- link into source detail

Source detail should show:

- source summary
- extracted decisions
- extracted action candidates
- related canonical action items
- raw evidence snippets where available

## Manual Actions

Manual add creates a canonical action item with `source_kind = manual`.

Future entry points:

- menu bar quick add
- Discord `/crumb add "..."`
- convert a Discord message into an action

Manual items can later merge with extracted items if the same real-world task appears in a source.
