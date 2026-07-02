# Future Product Ideas

This document collects product ideas that are not yet committed to implementation. The goal is to preserve the shape of the idea, expected user value, and early design notes so they can be revisited later.

Each section separates the original user-facing need from additional product notes.

## Action Grouping

### User Idea

Crumb should support grouping action items in two complementary ways.

The first is manual grouping. A user may want to correlate a set of action items into a named group, such as a specific project, initiative, or workstream. These groups are explicitly curated and should represent the user's own mental model rather than being tied only to where the items came from.

The second is optional grouping by pull request. When many action items come from one PR, it is useful to see them together rather than intermingled with unrelated PR work. This should be an optional view because the normal flat inbox is still useful for reviewing all open work sorted by added date, due date, priority, or other global criteria.

### Notes

Manual groups and PR groups should likely be treated as different concepts:

- Manual groups are persistent user-owned organization.
- PR grouping is a derived view over action metadata, probably based on PR URL or another stable PR identifier.

An initial UI shape could expose a view mode control in Actions:

- `Flat`
- `By PR`
- `By Group`

`Flat` should probably remain the default because Crumb's primary job is still answering "what do I need to do?" across sources. `By PR` and `By Group` would be alternate lenses rather than separate task systems.

Manual grouping can start with a single primary group per action item. That keeps grouping deterministic and avoids prematurely turning groups into tags. If users later need many-to-many organization, tags can be introduced separately.

In grouped views, each section could be collapsible and show compact metadata such as item count, earliest due date, newest activity, or highest priority. Items inside a group should still use the normal action row treatment and retain their existing lifecycle controls.

Open questions:

- Should manually created groups have their own archived/hidden state?
- Should a manual group be able to contain done or archived items, or only open items?
- Should `By PR` include a `No PR` section?
- Should grouping preference be persisted globally, per device, or only for the current session?
- How should grouping interact with assignee filters and dismissed/done views?

## Rich GitHub Pull Request Information

### User Idea

Crumb should show richer information about GitHub pull requests. The most important first step is showing the name of the pull request instead of only a PR number or URL.

Later, Crumb could show additional PR state such as whether CI is failing or whether there are comments that require responses. Those richer states likely require more frequent fetching, while the PR title can be heavily cached and fetched less aggressively.

This interacts with action grouping because PR groups could display richer labels and status information rather than only grouping under a bare PR number.

### Notes

This is a natural extension of the existing PR URL handling. A good initial version would separate stable PR metadata from live PR state:

- Stable metadata: title, repository owner/name, PR number, author, open/closed/merged state.
- Live state: CI/check status, review status, unresolved comments, requested changes, whether the user is requested as reviewer.

The first version should probably fetch only stable metadata and cache it aggressively. That would improve PR grouping labels and action row context without needing a background polling system for GitHub.

Live state can come later behind an explicit refresh, a low-frequency background refresh, or only for PRs attached to currently open action items. This avoids turning Crumb into a high-churn GitHub dashboard before the core action inbox model is ready.

Potential surfaces:

- Action row: show PR title in the source/link area.
- Expanded action detail: show repository, PR number, title, and open/merged state.
- `By PR` grouped view: use `repo#number: title` as the group heading.
- Future PR group heading: show compact status chips for CI, review comments, or requested changes.

Open questions:

- Should GitHub auth be required, or should public metadata be fetched unauthenticated when possible?
- Should PR metadata be stored as source evidence, as normalized linked-resource metadata, or directly on action items?
- How should Crumb handle renamed PRs?
- How much stale PR metadata is acceptable in a menu bar inbox?

## Multiple Links Per Action Item

### User Idea

Action items should be able to capture and display multiple relevant links.

For PR review action items, this is especially useful when multiple comments are relevant to the same PR. Instead of only linking to the overall PR, Crumb could preserve links to each individual comment so the user can jump directly to the right place.

### Notes

This should probably be modeled as an ordered set of links attached to an action item, rather than trying to pack every URL into a single field.

Useful link metadata could include:

- URL
- label
- kind, such as `pr`, `pr_comment`, `discord_message`, `asana_task`, or `manual`
- source/evidence reference when the link was extracted from a scrape
- first seen timestamp

The UI should stay compact. A first version could show one primary link in the collapsed row and expose additional links in the expanded detail. PR comment links may benefit from labels like `Comment 1`, `Comment 2`, or a short extracted snippet if available.

This also relates to evidence. Multiple links are not always separate tasks; sometimes they are separate pieces of context for the same task. Crumb should preserve them without over-splitting action items.

Open questions:

- Should duplicate links be deduped by normalized URL?
- Should links have user-editable labels?
- Should manually added links and extracted links look different?
- Should the primary link be selected automatically or manually?

## Manual Ordering, Priority, And Pins

### User Idea

Crumb should provide better ways to organize action item ordering.

Today, new action items can rise above more important existing work simply because they arrived later. For example, rebasing several PRs and receiving automated BugBot feedback can push those new items above manually added or higher-priority action items.

Possible organization tools include:

- Drag and drop reordering.
- Manually assigned priority.
- Pinning specific action items to the top.

A combination of these options would help make the action inbox better reflect what matters now. It is not yet clear how these should interact with grouping.

### Notes

These are related but distinct controls:

- Pinning answers "keep this visible."
- Priority answers "how important is this?"
- Manual ordering answers "what sequence do I want to see?"

The safest first version may be pinning, because it is easy to understand and has a predictable effect across sort modes: pinned open items appear before unpinned open items.

Priority could be added as a small fixed set, such as `None`, `Low`, `Medium`, `High`, or `Urgent`. Priority should influence default sorting but should not fully hide lower-priority items.

Drag and drop is powerful but trickier because Crumb has multiple filters and future grouped views. A manual order can be global, per group, or per current filtered view. Per-group manual ordering may be intuitive inside `By Group`, while global manual ordering may conflict with due date or priority sorting.

Potential sort model:

1. Pinned items first.
2. Then explicit priority.
3. Then due date.
4. Then recency/relevance.

Manual drag order could either override this model or be a separate sort mode called `Manual`. Keeping it as a sort mode may make the behavior easier to reason about.

Open questions:

- Should manual ordering apply only in `Flat` view or also within groups?
- Should pins appear at the top of every group, or only at the top of the full inbox?
- Should manually added actions receive a default priority boost?
- Should automated bot feedback be deprioritized unless explicitly assigned to the user?

## Asana Integration

### User Idea

An Asana integration would be valuable. Crumb could create Asana tickets from inside the UI and import Asana tickets as action items.

Bi-directional sync would be useful but may be complicated for an initial version.

### Notes

Asana fits Crumb's broader product direction because it turns external work-tracking systems into action inbox entries.

A practical rollout could be staged:

1. Import assigned Asana tasks into Crumb as action items.
2. Create an Asana task from an existing Crumb action item.
3. Link a Crumb action item to an existing Asana task.
4. Add selective sync of status, due date, and assignee.
5. Later, explore broader bi-directional sync.

The integration should be careful about source authority. Asana task completion can be more authoritative than Discord absence, but users may still want Crumb-specific lifecycle states such as snoozed or archived.

Potential UI surfaces:

- Action detail: `Create Asana task`.
- Action detail: linked Asana task status and due date.
- Sources: Asana workspace/project/task sources.
- Settings or source management: choose which projects or assigned tasks to watch.

Open questions:

- Should importing Asana tasks be limited to tasks assigned to the current user?
- Should Crumb status changes update Asana, or should Asana remain authoritative?
- How should Crumb avoid duplicating a Discord-derived action that already has an Asana task?
- Should Asana-created actions keep their Asana title as canonical, or can Crumb rewrite them?

## Manual Notes And Link Attachments

### User Idea

It would be useful to attach things to action items by hand. File attachments may be too tricky for an initial version because storing files has more complexity, but manual notes and links would be very helpful.

### Notes

Manual notes and links would let users enrich an action item without changing the original extracted evidence.

A simple first version could add:

- A freeform notes field on each action item.
- A list of manually attached links.

These should be distinct from extracted evidence. Evidence explains why Crumb believes the action exists; manual attachments are user-authored context for doing the work.

Manual links should probably use the same underlying link model as extracted links, with a `manual` kind or source marker. That would make the UI consistent and prepare for future link editing.

Open questions:

- Should notes support Markdown from the beginning?
- Should manual notes be searchable?
- Should edits be timestamped or versioned?
- Should notes be visible in the collapsed row, or only in expanded detail?

## Markdown Support In Action Items

### User Idea

Markdown support in action items would be useful.

### Notes

Markdown could apply to different fields with different risk levels:

- Manual notes are the best first candidate.
- Manually created action descriptions are also reasonable.
- Extracted action titles should probably remain plain text so the inbox stays compact and predictable.

A conservative Markdown implementation should support common formatting such as links, inline code, lists, and emphasis. It should avoid surprising layout in compact rows. Rendering Markdown only in expanded detail would preserve the scannability of the main inbox.

Markdown also interacts with manual links. If notes support Markdown links, Crumb should still consider whether those links should become structured attached links or remain embedded in the note text.

Open questions:

- Should Markdown be stored raw and rendered in the UI, or normalized into structured content?
- Should Markdown links be extracted into the action's link list?
- Should source-extracted content ever render as Markdown, or only user-authored content?

## CLI For Agents And Automation

### User Idea

A CLI would be a valuable addition so agents can interact with action items, view current action items, and understand what work remains.

The CLI could also summarize what the user should focus on right now. CLIs are a strong interface for agents.

### Notes

A CLI could make Crumb useful outside the menu bar UI and provide a stable integration surface for coding agents, scripts, and local automation.

Useful commands might include:

- `crumb actions list`
- `crumb actions show <id>`
- `crumb actions add`
- `crumb actions done <id>`
- `crumb actions archive <id>`
- `crumb actions note <id>`
- `crumb actions link add <id> <url>`
- `crumb focus`

The CLI should output both human-readable and machine-readable formats. JSON output would be especially useful for agents.

The CLI also needs a careful connection story. It could talk to the same SQLite database directly, call into the Tauri app if it is running, or use a small local IPC/API layer. Direct DB access may be simpler, but it risks bypassing business logic and event emission.

Open questions:

- Should the CLI require the app to be running?
- Should agent writes be allowed by default, or should they require confirmation?
- Should CLI-created actions be marked as `manual`, `cli`, or both?
- Should `crumb focus` be deterministic sorting, AI-assisted summarization, or both?

## Watched Source Indicators

### User Idea

The UI should indicate which sources are being automatically watched.

### Notes

This is a small but useful visibility improvement. If Crumb is watching a Discord channel or other future source, the Sources view should make that state obvious.

Potential surfaces:

- A watched indicator on source rows.
- A filter for watched versus unwatched sources.
- Source detail showing watch status, last poll time, and last seen message/time.
- Inline watch/unwatch controls if the source supports them.

This should stay informational and compact. The Sources view already carries scrape history and source-local context, so watch state should be easy to scan without making source rows too noisy.

Open questions:

- Should watch status be visible in Actions when an action came from a watched source?
- Should Crumb show when a watched source last produced a new action?
- Should watch failures or permission issues surface in the same indicator?
- How should this generalize beyond Discord to Asana, Notion, and manual sources?
