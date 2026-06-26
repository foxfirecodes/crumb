import { useEffect, useMemo, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  createManualActionItem,
  deleteSource,
  getScrape,
  getSidecarStatus,
  listActionItems,
  listScrapes,
  onActionsUpdated,
  onScrapeNew,
  onScrapeUpdated,
  onSidecarStatus,
  openActionSourceInDiscord,
  openSettingsWindow,
  setActionItemAssignee,
  setActionItemStatus,
} from "./lib/ipc";
import type {
  CanonicalActionItem,
  ActionItemStatusFilter,
  ActionItemSort,
  ScrapeDetail,
  ScrapeSummary,
  SidecarStatus,
} from "./lib/types";
import { ScrapeList } from "./components/ScrapeList";
import { ScrapeDetailView } from "./components/ScrapeDetail";
import { StatusDot } from "./components/StatusDot";
import { ActionList } from "./components/ActionList";

type View = "actions" | "sources";
const PERSON_FILTER_STORAGE_KEY = "crumb.personFilter";

interface StoredPersonFilter {
  key: string;
  label: string | null;
}

export default function App() {
  const [view, setView] = useState<View>("actions");
  const [actionStatusFilter, setActionStatusFilter] =
    useState<ActionItemStatusFilter>("open");
  const [actionSort, setActionSort] = useState<ActionItemSort>("newest");
  const [storedPersonFilter, setStoredPersonFilter] =
    useState<StoredPersonFilter>(readStoredPersonFilter);
  const [actions, setActions] = useState<CanonicalActionItem[]>([]);
  const [scrapes, setScrapes] = useState<ScrapeSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pendingDeleteId, setPendingDeleteId] = useState<string | null>(null);
  const [detail, setDetail] = useState<ScrapeDetail | null>(null);
  const [status, setStatus] = useState<SidecarStatus>({ kind: "starting" });
  const [manualAddOpen, setManualAddOpen] = useState(false);
  const personFilter = storedPersonFilter.key;
  const actionSortForStatus =
    actionStatusFilter === "dismissed" ? "newest" : actionSort;

  useEffect(() => {
    listActionItems(actionStatusFilter, actionSortForStatus)
      .then(setActions)
      .catch(console.error);
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
      listActionItems(actionStatusFilter, actionSortForStatus)
        .then(setActions)
        .catch(console.error);
    });
    const unlistenActions = onActionsUpdated(() => {
      listActionItems(actionStatusFilter, actionSortForStatus)
        .then(setActions)
        .catch(console.error);
    });
    const unlistenStatus = onSidecarStatus(setStatus);

    return () => {
      unlistenNew.then((fn) => fn());
      unlistenUpdated.then((fn) => fn());
      unlistenActions.then((fn) => fn());
      unlistenStatus.then((fn) => fn());
    };
  }, [actionStatusFilter, actionSortForStatus]);

  useEffect(() => {
    if (!selectedId) {
      setDetail(null);
      return;
    }
    getScrape(selectedId).then(setDetail).catch(console.error);
  }, [selectedId, scrapes]);

  useEffect(() => {
    if (!pendingDeleteId) return;
    const timeout = window.setTimeout(() => setPendingDeleteId(null), 5000);
    return () => window.clearTimeout(timeout);
  }, [pendingDeleteId]);

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
    const options = [...people.values()].sort((a, b) =>
      a.label.localeCompare(b.label),
    );
    if (
      storedPersonFilter.key !== "all" &&
      !options.some((person) => person.key === storedPersonFilter.key)
    ) {
      options.unshift({
        key: storedPersonFilter.key,
        label: storedPersonFilter.label ?? storedPersonFilter.key,
        count: 0,
      });
    }
    return options;
  }, [actions, storedPersonFilter]);

  const filteredActions =
    personFilter === "all"
      ? actions
      : actions.filter((action) => assigneeFilterKey(action) === personFilter);

  const refreshActions = () => {
    listActionItems(actionStatusFilter, actionSortForStatus)
      .then(setActions)
      .catch(console.error);
  };

  const refreshScrapes = () => {
    listScrapes().then(setScrapes).catch(console.error);
  };

  const changePersonFilter = (key: string) => {
    const next: StoredPersonFilter = {
      key,
      label:
        key === "all"
          ? null
          : personOptions.find((person) => person.key === key)?.label ?? key,
    };
    setStoredPersonFilter(next);
    writeStoredPersonFilter(next);
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

  const changeActionAssignee = (
    id: string,
    assignee: string | null,
    assigneeKey: string | null,
  ) => {
    setActionItemAssignee(id, assignee, assigneeKey)
      .then(refreshActions)
      .catch(console.error);
  };

  const addManualAction = async (title: string) => {
    const selectedAssignee =
      personFilter === "all"
        ? null
        : personOptions.find((person) => person.key === personFilter) ?? {
            key: personFilter,
            label: storedPersonFilter.label ?? personFilter,
            count: 0,
          };
    setActionStatusFilter("open");

    await createManualActionItem(
      title,
      selectedAssignee?.label ?? null,
      selectedAssignee?.key ?? null,
    );
    const nextActions = await listActionItems("open", actionSort);
    setActions(nextActions);
    setManualAddOpen(false);
  };

  const toggleManualAdd = () => {
    setSelectedId(null);
    setView("actions");
    setManualAddOpen((current) => (view === "actions" ? !current : true));
  };

  const openActionSource = (item: CanonicalActionItem) => {
    if (item.sourceKind !== "discord") return;
    setSelectedId(`${item.sourceKind}:${item.sourceScope}`);
    setView("sources");
  };

  const viewActionSource = (item: CanonicalActionItem) => {
    if (item.sourceKind !== "discord") return;
    openActionSourceInDiscord(item.id).catch(console.error);
  };

  const openActionUrl = (url: string) => {
    openUrl(url).catch(console.error);
  };

  const openSettings = () => {
    openSettingsWindow().catch(console.error);
  };

  const removeSource = (id: string) => {
    if (pendingDeleteId !== id) {
      setPendingDeleteId(id);
      return;
    }

    deleteSource(id)
      .then(() => {
        setPendingDeleteId(null);
        setSelectedId((current) => (current === id ? null : current));
        setDetail((current) =>
          current && current.scrape.id === id ? null : current,
        );
        setScrapes((current) => current.filter((scrape) => scrape.id !== id));
        refreshScrapes();
        refreshActions();
      })
      .catch((error) => {
        setPendingDeleteId(null);
        console.error(error);
      });
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
        <button
          className={
            manualAddOpen && view === "actions"
              ? "popover__add-action popover__add-action--active"
              : "popover__add-action"
          }
          onClick={toggleManualAdd}
          aria-label="Add action item"
          aria-expanded={manualAddOpen && view === "actions"}
          title="Add action item"
        >
          +
        </button>
        <button
          className="popover__settings"
          onClick={openSettings}
          aria-label="Settings"
          title="Settings"
        >
          <span className="popover__settings-icon" aria-hidden="true">
            ⚙
          </span>
        </button>
      </header>

      <main className="popover__body">
        {status.kind === "needssetup" ? (
          <div className="empty">
            <div>Setup needed</div>
            <div className="empty__hint">
              Missing {status.missing.join(", ")}
            </div>
            <button className="empty__action" onClick={openSettings}>
              Open Settings
            </button>
          </div>
        ) : view === "actions" ? (
          <ActionList
            actions={filteredActions}
            isManualAddOpen={manualAddOpen}
            statusFilter={actionStatusFilter}
            actionSort={actionSort}
            personFilter={personFilter}
            personOptions={personOptions}
            onStatusFilterChange={setActionStatusFilter}
            onActionSortChange={setActionSort}
            onPersonFilterChange={changePersonFilter}
            onSourceOpen={openActionSource}
            onSourceView={viewActionSource}
            onUrlOpen={openActionUrl}
            onAssigneeChange={changeActionAssignee}
            onManualAdd={addManualAction}
            onDismiss={dismissAction}
            onRestore={restoreAction}
          />
        ) : selectedId && detail ? (
          <ScrapeDetailView
            detail={detail}
            pendingDeleteId={pendingDeleteId}
            onBack={() => setSelectedId(null)}
            onDelete={removeSource}
          />
        ) : (
          <ScrapeList
            scrapes={scrapes}
            pendingDeleteId={pendingDeleteId}
            onSelect={setSelectedId}
            onDelete={removeSource}
          />
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

const readStoredPersonFilter = (): StoredPersonFilter => {
  try {
    const raw = window.localStorage.getItem(PERSON_FILTER_STORAGE_KEY);
    if (!raw) return { key: "all", label: null };
    const parsed = JSON.parse(raw) as Partial<StoredPersonFilter>;
    if (!parsed.key || typeof parsed.key !== "string") {
      return { key: "all", label: null };
    }
    return {
      key: parsed.key,
      label: typeof parsed.label === "string" ? parsed.label : null,
    };
  } catch {
    return { key: "all", label: null };
  }
};

const writeStoredPersonFilter = (filter: StoredPersonFilter) => {
  try {
    window.localStorage.setItem(
      PERSON_FILTER_STORAGE_KEY,
      JSON.stringify(filter),
    );
  } catch {
    // Local storage may be unavailable in some webview modes.
  }
};
