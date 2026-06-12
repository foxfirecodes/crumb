import { type FormEvent, useState } from "react";
import type {
  CanonicalActionItem,
  ActionItemStatusFilter,
  ActionItemSort,
} from "../lib/types";

interface PersonOption {
  key: string;
  label: string;
  count: number;
}

interface Props {
  actions: CanonicalActionItem[];
  statusFilter: ActionItemStatusFilter;
  actionSort: ActionItemSort;
  personFilter: string;
  personOptions: PersonOption[];
  onStatusFilterChange: (filter: ActionItemStatusFilter) => void;
  onActionSortChange: (sort: ActionItemSort) => void;
  onPersonFilterChange: (key: string) => void;
  onSourceOpen: (item: CanonicalActionItem) => void;
  onSourceView: (item: CanonicalActionItem) => void;
  onUrlOpen: (url: string) => void;
  onAssigneeChange: (
    id: string,
    assignee: string | null,
    assigneeKey: string | null,
  ) => void;
  onManualAdd: (title: string) => Promise<void>;
  onDismiss: (id: string) => void;
  onRestore: (id: string) => void;
}

const formatTime = (ts: number) => {
  const diff = Date.now() - ts;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return new Date(ts).toLocaleDateString();
};

export function ActionList({
  actions,
  statusFilter,
  actionSort,
  personFilter,
  personOptions,
  onStatusFilterChange,
  onActionSortChange,
  onPersonFilterChange,
  onSourceOpen,
  onSourceView,
  onUrlOpen,
  onAssigneeChange,
  onManualAdd,
  onDismiss,
  onRestore,
}: Props) {
  const isDismissed = statusFilter === "dismissed";
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [assigneeDrafts, setAssigneeDrafts] = useState<Record<string, string>>(
    {},
  );
  const [manualDraft, setManualDraft] = useState("");
  const [manualPending, setManualPending] = useState(false);
  const [manualError, setManualError] = useState<string | null>(null);
  const toggleExpanded = (id: string) => {
    setExpandedId((current) => (current === id ? null : id));
  };

  const submitManualAction = (event: FormEvent) => {
    event.preventDefault();
    const title = manualDraft.trim();
    if (!title || manualPending) return;

    setManualPending(true);
    setManualError(null);
    onManualAdd(title)
      .then(() => setManualDraft(""))
      .catch((error: unknown) => {
        setManualError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => setManualPending(false));
  };

  return (
    <div className="actions">
      <form className="actions__quick-add" onSubmit={submitManualAction}>
        <input
          value={manualDraft}
          placeholder="New action item"
          aria-label="New action item"
          onChange={(event) => setManualDraft(event.currentTarget.value)}
        />
        <button
          type="submit"
          disabled={!manualDraft.trim() || manualPending}
          aria-label="Add action item"
          title="Add action item"
        >
          {manualPending ? "Adding" : "Add"}
        </button>
        {manualError && (
          <div className="actions__quick-add-error" role="alert">
            {manualError}
          </div>
        )}
      </form>
      <div className="actions__filters">
        <div className="actions__status" aria-label="Action item status">
          <button
            className={
              statusFilter === "open"
                ? "actions__status-button actions__status-button--active"
                : "actions__status-button"
            }
            onClick={() => onStatusFilterChange("open")}
          >
            Open
          </button>
          <button
            className={
              statusFilter === "dismissed"
                ? "actions__status-button actions__status-button--active"
                : "actions__status-button"
            }
            onClick={() => onStatusFilterChange("dismissed")}
          >
            Dismissed
          </button>
        </div>

        <select
          className="actions__person"
          value={personFilter}
          onChange={(event) => onPersonFilterChange(event.currentTarget.value)}
          aria-label="Filter action items by person"
        >
          <option value="all">Everyone</option>
          {personOptions.map((person) => (
            <option key={person.key} value={person.key}>
              {person.label} ({person.count})
            </option>
          ))}
        </select>

        {!isDismissed && (
          <select
            className="actions__sort"
            value={actionSort}
            onChange={(event) =>
              onActionSortChange(event.currentTarget.value as ActionItemSort)
            }
            aria-label="Sort action items"
          >
            <option value="newest">Newest</option>
            <option value="due">Due</option>
          </select>
        )}
      </div>

      {actions.length === 0 ? (
        <div className="empty">
          <p>
            {isDismissed
              ? "No dismissed action items."
              : "No open action items."}
          </p>
          {!isDismissed && personFilter === "all" && (
            <p className="empty__hint">
              Run <code>/scrape</code> in Discord to extract tasks.
            </p>
          )}
        </div>
      ) : (
        <ul className="action-list">
          {actions.map((item) => (
            <li key={item.id} className="action-list__item">
              <div
                className="action-list__row"
                role="button"
                tabIndex={0}
                onClick={() => toggleExpanded(item.id)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" || event.key === " ") {
                    event.preventDefault();
                    toggleExpanded(item.id);
                  }
                }}
              >
                {isDismissed ? (
                  <button
                    className="action-list__restore"
                    title="Restore"
                    aria-label={`Restore "${item.title}"`}
                    onClick={(event) => {
                      event.stopPropagation();
                      onRestore(item.id);
                    }}
                    onKeyDown={(event) => event.stopPropagation()}
                  >
                    Restore
                  </button>
                ) : (
                  <button
                    className="action-list__check"
                    title="Dismiss"
                    aria-label={`Dismiss "${item.title}"`}
                    onClick={(event) => {
                      event.stopPropagation();
                      onDismiss(item.id);
                    }}
                    onKeyDown={(event) => event.stopPropagation()}
                  />
                )}
                <div className="action-list__body">
                  <div className="action-list__title-row">
                    <div className="action-list__title">{item.title}</div>
                  </div>
                  <div className="action-list__meta">
                    {item.assignee && <span>{item.assignee}</span>}
                    {item.evidenceCount > 1 && (
                      <span>{item.evidenceCount} sightings</span>
                    )}
                    {item.due && <span>Due {item.due}</span>}
                    <span>
                      {isDismissed
                        ? `Dismissed ${formatTime(item.completedAt ?? item.lastSeenAt)}`
                        : `Added ${formatTime(item.firstSeenAt)}`}
                    </span>
                    {item.sourceKind === "discord" && (
                      <span>
                        <button
                          className="action-list__meta-link"
                          onClick={(event) => {
                            event.preventDefault();
                            event.stopPropagation();
                            onSourceView(item);
                          }}
                          onKeyDown={(event) => event.stopPropagation()}
                        >
                          View
                        </button>
                      </span>
                    )}
                    {item.url && (
                      <span>
                        <a
                          className="action-list__meta-link"
                          href={item.url}
                          target="_blank"
                          rel="noreferrer"
                          onClick={(event) => {
                            event.preventDefault();
                            event.stopPropagation();
                            const url = item.url;
                            if (url) onUrlOpen(url);
                          }}
                          onKeyDown={(event) => event.stopPropagation()}
                        >
                          PR
                        </a>
                      </span>
                    )}
                  </div>
                </div>
                <button
                  className="action-list__expand"
                  title={expandedId === item.id ? "Collapse" : "Expand details"}
                  aria-label={
                    expandedId === item.id
                      ? `Collapse "${item.title}"`
                      : `Expand "${item.title}"`
                  }
                  onClick={(event) => {
                    event.stopPropagation();
                    toggleExpanded(item.id);
                  }}
                  onKeyDown={(event) => event.stopPropagation()}
                >
                  {expandedId === item.id ? "▾" : "▸"}
                </button>
              </div>
              {expandedId === item.id && (
                <ActionItemDetails
                  item={item}
                  assigneeDraft={assigneeDrafts[item.id] ?? item.assignee ?? ""}
                  personOptions={personOptions}
                  onAssigneeDraftChange={(value) =>
                    setAssigneeDrafts((drafts) => ({
                      ...drafts,
                      [item.id]: value,
                    }))
                  }
                  onAssigneeSave={() => {
                    const assignee = (
                      assigneeDrafts[item.id] ??
                      item.assignee ??
                      ""
                    ).trim();
                    const option = personOptions.find(
                      (person) =>
                        person.label.toLowerCase() === assignee.toLowerCase(),
                    );
                    onAssigneeChange(
                      item.id,
                      assignee || null,
                      assignee ? option?.key ?? null : null,
                    );
                  }}
                  onSourceOpen={() => onSourceOpen(item)}
                  onUrlOpen={onUrlOpen}
                />
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ActionItemDetails({
  item,
  assigneeDraft,
  personOptions,
  onAssigneeDraftChange,
  onAssigneeSave,
  onSourceOpen,
  onUrlOpen,
}: {
  item: CanonicalActionItem;
  assigneeDraft: string;
  personOptions: PersonOption[];
  onAssigneeDraftChange: (value: string) => void;
  onAssigneeSave: () => void;
  onSourceOpen: () => void;
  onUrlOpen: (url: string) => void;
}) {
  const assigneeChanged = assigneeDraft.trim() !== (item.assignee ?? "");

  return (
    <div className="action-list__details">
      <div className="action-list__detail-row">
        <span>Source</span>
        {item.sourceKind === "discord" ? (
          <button className="action-list__source" onClick={onSourceOpen}>
            {item.sourceLabel ?? item.sourceKind}
          </button>
        ) : (
          <span className="action-list__source-label">
            {item.sourceLabel ?? item.sourceKind}
          </span>
        )}
      </div>
      {item.url && (
        <div className="action-list__detail-row">
          <span>PR</span>
          <a
            className="action-list__source"
            href={item.url}
            target="_blank"
            rel="noreferrer"
            onClick={(event) => {
              event.preventDefault();
              const url = item.url;
              if (url) onUrlOpen(url);
            }}
          >
            {item.url}
          </a>
        </div>
      )}
      <div className="action-list__detail-grid">
        <div>
          <span>Due</span>
          <strong>{item.due || "No due date"}</strong>
        </div>
        <div>
          <span>Status</span>
          <strong>{item.status}</strong>
        </div>
        <div>
          <span>First seen</span>
          <strong>{formatTime(item.firstSeenAt)}</strong>
        </div>
        <div>
          <span>Last seen</span>
          <strong>{formatTime(item.lastSeenAt)}</strong>
        </div>
      </div>
      {item.latestContext && (
        <div className="action-list__context">{item.latestContext}</div>
      )}
      <div className="action-list__assignee">
        <label htmlFor={`assignee-${item.id}`}>Assignee</label>
        <input
          id={`assignee-${item.id}`}
          list="action-assignees"
          value={assigneeDraft}
          placeholder="Unassigned"
          onChange={(event) => onAssigneeDraftChange(event.currentTarget.value)}
        />
        <datalist id="action-assignees">
          {personOptions.map((person) => (
            <option key={person.key} value={person.label} />
          ))}
        </datalist>
        <button disabled={!assigneeChanged} onClick={onAssigneeSave}>
          Save
        </button>
      </div>
    </div>
  );
}
