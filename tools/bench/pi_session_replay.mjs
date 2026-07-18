// M6 family (b), pi side: session-replay throughput (best-effort).
//
// The pi counterpart to tau's `tau_session_replay.py` and rho's Criterion
// `session_replay` bench. It parses every JSONL entry line and replays the log
// into pi's runtime message list — the load path pi runs when opening a
// session — using the INSTALLED pi's own session code
// (`parseSessionEntries` + `buildSessionContext` from
// @earendil-works/pi-coding-agent), i.e. the exact code the measured binary
// runs.
//
// IMPORTANT — same workload, different format. pi's session file format is NOT
// tau/rho's: entries form an id/parentId tree of typed records ("message",
// "compaction", "model_change", …), not tau/rho's `SessionEntry` union. We
// therefore replay pi's OWN format over the SAME entry counts as tau/rho's
// `linear` trees (a single parent→child chain of alternating user/assistant
// message entries), and compare parse+replay throughput at equal N. The
// deep-branch and compaction-heavy variants are intentionally NOT ported: pi's
// tree/compaction semantics differ enough that a synthetic port would compare
// two different algorithms, not the same one. Documented, not silently dropped.
//
// Import dirs come from PI_AI_DIST / PI_CA_DIST (resolved from the real pi
// entry) so we measure the shipped code, never a rebuild.
//
// Usage: PI_CA_DIST=.../pi-coding-agent/dist node tools/bench/pi_session_replay.mjs \
//          --out tools/bench/results/pi_session_replay.json [--scale 1.0]

import { performance } from "node:perf_hooks";

const CA = process.env.PI_CA_DIST;
if (!CA) {
  console.error("error: set PI_CA_DIST to the installed pi-coding-agent dist dir");
  process.exit(2);
}
const { parseSessionEntries, buildSessionContext } = await import(`${CA}/core/session-manager.js`);

// Match tau's per-size (warmup, measured) so the two engines meet the same way.
const SIZES = { "1k": [1000, 3, 30], "10k": [10000, 2, 10], "100k": [100000, 1, 3] };

function parseArgs() {
  const args = process.argv.slice(2);
  let out = "-";
  let scale = 1.0;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--out") out = args[++i];
    else if (args[i] === "--scale") scale = Number.parseFloat(args[++i]);
  }
  return { out, scale };
}

// Build a linear pi session (one parent→child chain of N message entries,
// alternating user/assistant) as a JSONL string — the pi-format analogue of
// tau/rho's `linear-<N>` synthetic tree.
function buildLinearSession(n) {
  const lines = new Array(n);
  let prev = null;
  for (let i = 0; i < n; i++) {
    const id = `e${i}`;
    const isAssistant = i % 2 === 1;
    const message = isAssistant
      ? {
          role: "assistant",
          content: [{ type: "text", text: `reply ${i}` }],
          api: "faux",
          provider: "faux",
          model: "faux-1",
          usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
          stopReason: "stop",
        }
      : { role: "user", content: [{ type: "text", text: `message ${i}` }] };
    lines[i] = JSON.stringify({ type: "message", id, parentId: prev, timestamp: 0, message });
    prev = id;
  }
  return lines.join("\n");
}

function summarize(timesMs) {
  const mean = timesMs.reduce((a, b) => a + b, 0) / timesMs.length;
  const variance = timesMs.length > 1 ? timesMs.reduce((a, b) => a + (b - mean) ** 2, 0) / (timesMs.length - 1) : 0;
  return { mean_ms: mean, stddev_ms: Math.sqrt(variance), min_ms: Math.min(...timesMs), iterations: timesMs.length };
}

const { out, scale } = parseArgs();
const records = [];
for (const [size, [n, warmup, measured]] of Object.entries(SIZES)) {
  const content = buildLinearSession(n);
  const w = Math.max(1, Math.round(warmup * scale));
  const m = Math.max(1, Math.round(measured * scale));

  const loadOnce = () => buildSessionContext(parseSessionEntries(content)).messages.length;
  for (let i = 0; i < w; i++) loadOnce();
  const times = [];
  for (let i = 0; i < m; i++) {
    const t0 = performance.now();
    loadOnce();
    times.push(performance.now() - t0);
  }
  const stats = summarize(times);
  const dataset = `linear-${size}`;
  process.stderr.write(`  pi session_replay ${dataset}: ${n} entries\n`);
  records.push({
    family: "session_replay",
    impl: "pi",
    dataset,
    n_entries: n,
    entries_per_sec: stats.mean_ms > 0 ? n / (stats.mean_ms / 1e3) : null,
    ...stats,
  });
}

const payload = JSON.stringify(records, null, 2);
if (out && out !== "-") {
  const { writeFileSync } = await import("node:fs");
  writeFileSync(out, payload + "\n");
  process.stderr.write(`wrote ${records.length} records -> ${out}\n`);
} else {
  process.stdout.write(payload + "\n");
}
