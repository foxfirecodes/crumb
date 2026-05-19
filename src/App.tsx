import { useEffect, useMemo, useState } from "react";
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
  ActionItemStatusFilter,
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
  const [actionStatusFilter, setActionStatusFilter] =
    useState<ActionItemStatusFilter>("open");
  const [personFilter, setPersonFilter] = useState<string>("all");
  const [actions, setActions] = useState<CanonicalActionItem[]>([]);
  const [scrapes, setScrapes] = useState<ScrapeSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ScrapeDetail | null>(null);
  const [status, setStatus] = useState<SidecarStatus>({ kind: "starting" });

  useEffect(() => {
    listActionItems(actionStatusFilter).then(setActions).catch(console.error);
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
      listActionItems(actionStatusFilter).then(setActions).catch(console.error);
    });
    const unlistenActions = onActionsUpdated(() => {
      listActionItems(actionStatusFilter).then(setActions).catch(console.error);
    });
    const unlistenStatus = onSidecarStatus(setStatus);

    return () => {
      unlistenNew.then((fn) => fn());
      unlistenUpdated.then((fn) => fn());
      unlistenActions.then((fn) => fn());
      unlistenStatus.then((fn) => fn());
    };
  }, [actionStatusFilter]);

  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    getScrape(selectedId).then(setDetail).catch(console.error);
  }, [selectedId, scrapes]);

  const personOptions = useMemo(() => {
    const people = new Map<string, { key: string; label: string; count: number }>();
    for (const action of actions) {
      const key = assigneeFilterKey(action);
      if (!key || !action.assignee) continue;
      const existing = people.get(key);
      if (existing) {
        existing.count += 1;
      } else {
        people.set(key, { key, label: action.assignee, count: 1 });
      }
    }
    return [...people.values()].sort((a, b) => a.label.localeCompare(b.label));
  }, [actions]);

  useEffect(() => {
    if (
      personFilter !== "all" &&
      !personOptions.some((person) => person.key === personFilter)
    ) {
      setPersonFilter("all");
    }
  }, [personFilter, personOptions]);

  const filteredActions =
    personFilter === "all"
      ? actions
      : actions.filter((action) => assigneeFilterKey(action) === personFilter);

  const refreshActions = () => {
    listActionItems(actionStatusFilter).then(setActions).catch(console.error);
  };

  const dismissAction = (id: string) => {
    setActionItemStatus(id, "done")
      .then(refreshActions)
      .catch(console.error);
  };

  const restoreAction = (id: string) => {
    setActionItemStatus(id, "inbox")
      .then(refreshActions)
      .catch(console.error);
  };

  return (
    <div className="popover">
      <header className="popover__header">
        <StatusDot status={status} />
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
      </header>

      <main className="popover__body">
        {view === "actions" ? (
          <ActionList
            actions={filteredActions}
            statusFilter={actionStatusFilter}
            personFilter={personFilter}
            personOptions={personOptions}
            onStatusFilterChange={setActionStatusFilter}
            onPersonFilterChange={setPersonFilter}
            onDismiss={dismissAction}
            onRestore={restoreAction}
          />
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

const assigneeFilterKey = (item: CanonicalActionItem) => {
  if (item.assigneeKey) return item.assigneeKey;
  if (!item.assignee) return null;
  const normalized = item.assignee
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
  return normalized ? `person:${normalized}` : null;
};
