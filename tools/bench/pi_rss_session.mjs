// M6 family (d), pi side: a scripted N-turn faux-provider session.
//
// The pi counterpart to tau's `tau_rss_session.py` and rho's
// `examples/rss_session.rs`. Drives pi's own `Agent` (from the INSTALLED
// @earendil-works/pi-agent-core, i.e. the exact code the measured `pi` binary
// runs) through N turns against pi's in-process faux provider
// (`createFauxCore`, pi's FakeProvider analogue) — no network, no tools — so the
// transcript accumulates in memory. It prints only the final transcript size;
// peak RSS is captured by the caller running it under `/usr/bin/time -l`
// (see tools/bench/rss.sh).
//
// Import paths for pi's bundled internals are supplied by the caller via
// PI_AI_DIST / PI_AGENT_DIST (resolved from the real `pi` entry point) so the
// harness measures the installed binary's own agent core, never a rebuilt copy.
//
// Usage: PI_AI_DIST=.../pi-ai/dist PI_AGENT_DIST=.../pi-agent-core/dist \
//        node tools/bench/pi_rss_session.mjs [turns]

const AI = process.env.PI_AI_DIST;
const AGENT = process.env.PI_AGENT_DIST;
if (!AI || !AGENT) {
  console.error("error: set PI_AI_DIST and PI_AGENT_DIST to the installed pi dist dirs");
  process.exit(2);
}

const { createFauxCore, fauxAssistantMessage } = await import(`${AI}/providers/faux.js`);
const { Agent } = await import(`${AGENT}/index.js`);

const turns = Number.parseInt(process.argv[2] ?? "500", 10);

// pi's FakeProvider analogue: a faux core whose streamSimple yields a scripted
// assistant message per call. Zero token size so streaming bookkeeping is
// trivial and does not dominate; the point is the accumulating transcript.
const faux = createFauxCore({ provider: "faux", tokenSize: { min: 1, max: 1 } });
faux.setResponses(Array.from({ length: turns }, (_, i) => fauxAssistantMessage(`reply ${i}`)));
const model = faux.getModel();

const agent = new Agent({
  initialState: { model, systemPrompt: "You are rho." },
  streamFn: faux.streamSimple,
});

// Each prompt is a fresh user turn; the message list accumulates across all N
// provider calls (the counterpart to tau's prompt()+continue_() loop).
for (let i = 0; i < turns; i++) {
  await agent.prompt(`go ${i}`);
}

console.log(`turns=${turns} messages=${agent.state.messages.length}`);
