// Newline-delimited JSON protocol over stdio.
// stdout = events + responses to the Rust shell.
// stderr = free-form logging (host renders nothing from it).

import type { HostRequest, HostResponse, SidecarEvent } from "./types";

type RequestHandler = (req: HostRequest) => Promise<unknown>;

export class Host {
  private handler: RequestHandler | null = null;
  private buffer = "";

  onRequest(handler: RequestHandler) {
    this.handler = handler;
  }

  emit(event: SidecarEvent) {
    process.stdout.write(JSON.stringify(event) + "\n");
  }

  log(level: "info" | "warn" | "error" | "debug", msg: string) {
    process.stderr.write(`[${level}] ${msg}\n`);
    this.emit({ kind: "log", level, msg });
  }

  start() {
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => this.ingest(chunk.toString()));
    process.stdin.on("end", () => {
      this.log("info", "stdin closed, exiting");
      process.exit(0);
    });
  }

  private ingest(chunk: string) {
    this.buffer += chunk;
    let idx: number;
    while ((idx = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (line) this.dispatch(line);
    }
  }

  private async dispatch(line: string) {
    let req: HostRequest;
    try {
      req = JSON.parse(line) as HostRequest;
    } catch (e) {
      this.log("error", `invalid JSON from host: ${(e as Error).message}`);
      return;
    }

    if (!this.handler) {
      this.respondError(req.id, "no handler registered");
      return;
    }

    try {
      const result = await this.handler(req);
      this.respondOk(req.id, result);
    } catch (e) {
      this.respondError(req.id, (e as Error).message);
    }
  }

  private respondOk(id: string, result: unknown) {
    const r: HostResponse = { id, ok: true, result };
    process.stdout.write(JSON.stringify(r) + "\n");
  }

  private respondError(id: string, error: string) {
    const r: HostResponse = { id, ok: false, error };
    process.stdout.write(JSON.stringify(r) + "\n");
  }
}
