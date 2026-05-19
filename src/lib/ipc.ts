import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { ScrapeDetail, ScrapeSummary, SidecarStatus } from "./types";

export const listScrapes = () => invoke<ScrapeSummary[]>("list_scrapes");
export const getScrape = (id: string) =>
  invoke<ScrapeDetail | null>("get_scrape", { id });
export const getSidecarStatus = () => invoke<SidecarStatus>("get_sidecar_status");
export const hidePopover = () => invoke<void>("hide_popover");

export const onScrapeNew = (cb: (s: ScrapeSummary) => void): Promise<UnlistenFn> =>
  listen<ScrapeSummary>("scrape:new", (e) => cb(e.payload));

export const onScrapeUpdated = (
  cb: (s: ScrapeSummary) => void,
): Promise<UnlistenFn> =>
  listen<ScrapeSummary>("scrape:updated", (e) => cb(e.payload));

export const onSidecarStatus = (
  cb: (s: SidecarStatus) => void,
): Promise<UnlistenFn> =>
  listen<SidecarStatus>("sidecar:status", (e) => cb(e.payload));
