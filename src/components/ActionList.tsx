import type { CanonicalActionItem } from "../lib/types";

interface Props {
  actions: CanonicalActionItem[];
  onDone: (id: string) => void;
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

export function ActionList({ actions, onDone }: Props) {
  if (actions.length === 0) {
    return (
      <div className="empty">
        <p>No open action items.</p>
        <p className="empty__hint">
          Run <code>/scrape</code> in Discord to extract tasks.
        </p>
      </div>
    );
  }

  return (
    <ul className="action-list">
      {actions.map((item) => (
        <li key={item.id} className="action-list__item">
          <button
            className="action-list__check"
            title="Mark done"
            aria-label={`Mark "${item.title}" done`}
            onClick={() => onDone(item.id)}
          />
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
  );
}
