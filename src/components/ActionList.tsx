import type {
  CanonicalActionItem,
  ActionItemStatusFilter,
} from "../lib/types";

interface Props {
  actions: CanonicalActionItem[];
  statusFilter: ActionItemStatusFilter;
  personFilter: string;
  personOptions: Array<{ key: string; label: string; count: number }>;
  onStatusFilterChange: (filter: ActionItemStatusFilter) => void;
  onPersonFilterChange: (key: string) => void;
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
  personFilter,
  personOptions,
  onStatusFilterChange,
  onPersonFilterChange,
  onDismiss,
  onRestore,
}: Props) {
  const isDismissed = statusFilter === "dismissed";

  return (
    <div className="actions">
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
              {isDismissed ? (
                <button
                  className="action-list__restore"
                  title="Restore"
                  aria-label={`Restore "${item.title}"`}
                  onClick={() => onRestore(item.id)}
                >
                  Restore
                </button>
              ) : (
                <button
                  className="action-list__check"
                  title="Dismiss"
                  aria-label={`Dismiss "${item.title}"`}
                  onClick={() => onDismiss(item.id)}
                />
              )}
              <div className="action-list__body">
                <div className="action-list__title">{item.title}</div>
                <div className="action-list__meta">
                  <span>{item.sourceLabel ?? item.sourceKind}</span>
                  {item.assignee && <span>{item.assignee}</span>}
                  {item.due && <span>due {item.due}</span>}
                  {item.evidenceCount > 1 && (
                    <span>{item.evidenceCount} sightings</span>
                  )}
                  <span>{formatTime(item.lastSeenAt)}</span>
                </div>
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
