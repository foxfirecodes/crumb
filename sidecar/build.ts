// Compiles the sidecar to a single static executable that Tauri bundles.
//
// Tauri's externalBin mechanism expects the binary name to be suffixed with
// the host triple, e.g. `crumb-sidecar-aarch64-apple-darwin`. We mirror that
// here so the file lands in the right place.

import { $, type BunPlugin } from "bun";
import { existsSync, mkdirSync } from "node:fs";
import { resolve } from "node:path";

const triple = (await $`rustc -vV`.text())
  .split("\n")
  .find((l) => l.startsWith("host:"))
  ?.replace("host:", "")
  .trim();

if (!triple) {
  console.error("Could not determine rustc host triple. Is rustc installed?");
  process.exit(1);
}

const outDir = resolve(import.meta.dir, "..", "src-tauri", "binaries");
if (!existsSync(outDir)) mkdirSync(outDir, { recursive: true });

const outFile = resolve(outDir, `crumb-sidecar-${triple}`);

// discord.js-selfbot-v13 transitively requires voice-stack modules at module
// load time. We don't do voice, so we replace those imports with a no-op
// shim. Anything that actually invokes voice code paths will fail loudly at
// runtime, which is fine — we never go down those paths.
const VOICE_STUBS = [
  "prism-media",
  "ffmpeg-static",
  "node-opus",
  "opusscript",
  "@discordjs/opus",
  "@discordjs/voice",
  "sodium",
  "libsodium-wrappers",
  "tweetnacl",
  "node-vad",
  "erlpack",
];

const STUB_SOURCE = `
const handler = {
  get(target, prop) {
    if (prop === '__esModule') return true;
    if (prop === 'default') return target;
    if (prop in target) return target[prop];
    // Lazily synthesize whatever the consumer asks for so destructured
    // imports don't crash at module load.
    const fn = function () { throw new Error('voice/audio support is stubbed out in crumb-sidecar'); };
    return new Proxy(fn, handler);
  },
};
const stub = new Proxy({}, handler);
module.exports = stub;
module.exports.default = stub;
`;

const stubPlugin: BunPlugin = {
  name: "voice-stub",
  setup(build) {
    const filter = new RegExp(
      `^(${VOICE_STUBS.map((s) => s.replace(/[.*+?^${}()|[\]\\\/]/g, "\\$&")).join("|")})(/.*)?$`,
    );
    build.onResolve({ filter }, (args) => ({
      path: args.path,
      namespace: "voice-stub",
    }));
    build.onLoad({ filter: /.*/, namespace: "voice-stub" }, () => ({
      contents: STUB_SOURCE,
      loader: "js",
    }));
  },
};

console.log(`▶ building sidecar → ${outFile}`);

const result = await Bun.build({
  entrypoints: [resolve(import.meta.dir, "src/index.ts")],
  outdir: outDir,
  naming: `crumb-sidecar-${triple}`,
  target: "bun",
  plugins: [stubPlugin],
  compile: {
    target: `bun-${triple.includes("aarch64") ? "darwin-arm64" : "darwin-x64"}`,
    outfile: outFile,
  },
  external: [
    // Native bindings we never need; if discord.js tries to load these they
    // fall back to JS implementations.
    "zlib-sync",
    "bufferutil",
    "utf-8-validate",
  ],
});

if (!result.success) {
  console.error("✗ build failed");
  for (const log of result.logs) console.error(log);
  process.exit(1);
}

await $`chmod +x ${outFile}`;
console.log("✓ sidecar built");
