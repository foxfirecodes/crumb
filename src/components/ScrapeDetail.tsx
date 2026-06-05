import type { ScrapeDetail } from "../lib/types";

interface Props {
  detail: ScrapeDetail;
  pendingDeleteId: string | null;
  onBack: () => void;
  onDelete: (id: string) => void;
}

export function ScrapeDetailView({
  detail,
  pendingDeleteId,
  onBack,
  onDelete,
}: Props) {
  const { scrape, decisions, actionItems } = detail;
  const isConfirmingDelete = pendingDeleteId === scrape.id;

  return (
    <div className="detail">
      <div className="detail__toolbar">
        <button className="detail__back" onClick={onBack}>
          ← back
        </button>
        <button
          className={
            isConfirmingDelete
              ? "detail__delete detail__delete--confirm"
              : "detail__delete"
          }
          onClick={() => onDelete(scrape.id)}
        >
          {isConfirmingDelete ? "Confirm delete" : "Delete source"}
        </button>
      </div>

      <h2 className="detail__title">
        {scrape.guildName ? `${scrape.guildName} · ` : ""}
        {scrape.channelName ?? scrape.channelId}
      </h2>

      {scrape.summary && <p className="detail__summary">{scrape.summary}</p>}
      {scrape.status === "failed" && scrape.error && (
        <section className="detail__error" aria-label="Source error details">
          <h3>Error details</h3>
          <pre>{scrape.error}</pre>
        </section>
      )}

      <section className="detail__section">
        <h3>Decisions ({decisions.length})</h3>
        {decisions.length === 0 ? (
          <p className="detail__empty">No decisions found.</p>
        ) : (
          <ul className="detail__list">
            {decisions.map((d) => (
              <li key={d.id}>
                <div className="detail__text">{d.text}</div>
                {d.context && <div className="detail__context">"{d.context}"</div>}
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="detail__section">
        <h3>Action items ({actionItems.length})</h3>
        {actionItems.length === 0 ? (
          <p className="detail__empty">No action items found.</p>
        ) : (
          <ul className="detail__list">
            {actionItems.map((a) => (
              <li key={a.id}>
                <div className="detail__text">{a.text}</div>
                <div className="detail__meta">
                  {a.assignee && <span>{a.assignee}</span>}
                  {a.due && <span>· {a.due}</span>}
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
