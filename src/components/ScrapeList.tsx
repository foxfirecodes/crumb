import type { ScrapeSummary } from "../lib/types";

interface Props {
  scrapes: ScrapeSummary[];
  onSelect: (id: string) => void;
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

const STATUS_GLYPH: Record<ScrapeSummary["status"], string> = {
  running: "◐",
  extracted: "●",
  failed: "✕",
};

export function ScrapeList({ scrapes, onSelect }: Props) {
  if (scrapes.length === 0) {
    return (
      <div className="empty">
        <p>No scrapes yet.</p>
        <p className="empty__hint">
          Run <code>/scrape</code> in any Discord channel.
        </p>
      </div>
    );
  }

  return (
    <ul className="scrape-list">
      {scrapes.map((s) => (
        <li
          key={s.id}
          className={`scrape-list__item scrape-list__item--${s.status}`}
          onClick={() => onSelect(s.id)}
        >
          <div className="scrape-list__row1">
            <span className={`scrape-list__glyph scrape-list__glyph--${s.status}`}>
              {STATUS_GLYPH[s.status]}
            </span>
            <span className="scrape-list__channel">
              {s.guildName ? `${s.guildName} · ` : ""}
              {s.channelName ?? s.channelId}
            </span>
            <span className="scrape-list__time">{formatTime(s.triggeredAt)}</span>
          </div>
          {s.summary && (
            <div className="scrape-list__summary">{s.summary}</div>
          )}
          {s.status === "failed" && s.error && (
            <div className="scrape-list__error">{s.error}</div>
          )}
        </li>
      ))}
    </ul>
  );
}
