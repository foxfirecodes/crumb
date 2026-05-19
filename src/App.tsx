import { useEffect, useState } from "react";
import {
  getScrape,
  getSidecarStatus,
  listActionItems,
  listScrapes,
  onActionsUpdated,
  onScrapeNew,
  onScrapeUpdated,
  onSidecarStatus,
  setActionItemStatus,
} from "./lib/ipc";
import type {
  CanonicalActionItem,
  ScrapeDetail,
  ScrapeSummary,
  SidecarStatus,
} from "./lib/types";
import { ScrapeList } from "./components/ScrapeList";
import { ScrapeDetailView } from "./components/ScrapeDetail";
import { StatusDot } from "./components/StatusDot";
import { ActionList } from "./components/ActionList";

type View = "actions" | "sources";

export default function App() {
  const [view, setView] = useState<View>("actions");
  const [actions, setActions] = useState<CanonicalActionItem[]>([]);
  const [scrapes, setScrapes] = useState<ScrapeSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ScrapeDetail | null>(null);
  const [status, setStatus] = useState<SidecarStatus>({ kind: "starting" });

  useEffect(() => {
    listActionItems().then(setActions).catch(console.error);
    listScrapes().then(setScrapes).catch(console.error);
    getSidecarStatus().then(setStatus).catch(console.error);

    const unlistenNew = onScrapeNew((s) => {
      setScrapes((prev) => [s, ...prev.filter((p) => p.id !== s.id)]);
    });
    const unlistenUpdated = onScrapeUpdated((s) => {
      setScrapes((prev) => prev.map((p) => (p.id === s.id ? s : p)));
      setDetail((prev) =>
        prev && prev.scrape.id === s.id ? { ...prev, scrape: s } : prev,
      );
      listActionItems().then(setActions).catch(console.error);
    });
    const unlistenActions = onActionsUpdated(setActions);
    const unlistenStatus = onSidecarStatus(setStatus);

    return () => {
      unlistenNew.then((fn) => fn());
      unlistenUpdated.then((fn) => fn());
      unlistenActions.then((fn) => fn());
      unlistenStatus.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    getScrape(selectedId).then(setDetail).catch(console.error);
  }, [selectedId, scrapes]);

  const markDone = (id: string) => {
    setActionItemStatus(id, "done")
      .then(() => listActionItems().then(setActions))
      .catch(console.error);
  };

  return (
    <div className="popover">
      <header className="popover__header">
        <div className="popover__heading">
          <span className="popover__title">Crumb</span>
          <nav className="popover__tabs">
            <button
              className={view === "actions" ? "popover__tab popover__tab--active" : "popover__tab"}
              onClick={() => {
                setView("actions");
                setSelectedId(null);
              }}
            >
              Actions
            </button>
            <button
              className={view === "sources" ? "popover__tab popover__tab--active" : "popover__tab"}
              onClick={() => setView("sources")}
            >
              Sources
            </button>
          </nav>
        </div>
        <StatusDot status={status} />
      </header>

      <main className="popover__body">
        {view === "actions" ? (
          <ActionList actions={actions} onDone={markDone} />
        ) : selectedId && detail ? (
          <ScrapeDetailView
            detail={detail}
            onBack={() => setSelectedId(null)}
          />
        ) : (
          <ScrapeList scrapes={scrapes} onSelect={setSelectedId} />
        )}
      </main>
    </div>
  );
}
