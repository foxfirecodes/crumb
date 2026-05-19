import type { SidecarStatus } from "../lib/types";

const COLOR: Record<SidecarStatus["kind"], string> = {
  starting: "#9ca3af",
  connected: "#10b981",
  disconnected: "#f59e0b",
  error: "#ef4444",
};

const LABEL: Record<SidecarStatus["kind"], string> = {
  starting: "starting",
  connected: "connected",
  disconnected: "disconnected",
  error: "error",
};

export function StatusDot({ status }: { status: SidecarStatus }) {
  const title =
    status.kind === "error"
      ? `error: ${status.message}`
      : status.kind === "connected"
        ? `connected${status.botUser ? ` as ${status.botUser}` : ""}`
        : LABEL[status.kind];

  return (
    <span className="status" title={title}>
      <span
        className="status__dot"
        style={{ background: COLOR[status.kind] }}
      />
      <span className="status__label">{LABEL[status.kind]}</span>
    </span>
  );
}
