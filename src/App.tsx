import { useEffect, useState } from "react";
import {
  getScrape,
  getSidecarStatus,
  listScrapes,
  onScrapeNew,
  onScrapeUpdated,
  onSidecarStatus,
} from "./lib/ipc";
import type {
  ScrapeDetail,
  ScrapeSummary,
  SidecarStatus,
} from "./lib/types";
import { ScrapeList } from "./components/ScrapeList";
import { ScrapeDetailView } from "./components/ScrapeDetail";
import { StatusDot } from "./components/StatusDot";

export default function App() {
  const [scrapes, setScrapes] = useState<ScrapeSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ScrapeDetail | null>(null);
  const [status, setStatus] = useState<SidecarStatus>({ kind: "starting" });

  useEffect(() => {
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
    });
    const unlistenStatus = onSidecarStatus(setStatus);

    return () => {
      unlistenNew.then((fn) => fn());
      unlistenUpdated.then((fn) => fn());
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

  return (
    <div className="popover">
      <header className="popover__header">
        <span className="popover__title">Crumb</span>
        <StatusDot status={status} />
      </header>

      <main className="popover__body">
        {selectedId && detail ? (
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
