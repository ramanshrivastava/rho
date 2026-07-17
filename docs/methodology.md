# How rho is built: milestone-gated multi-agent development

rho is a full-parity Rust port of [tau](https://github.com/huggingface/tau),
written almost entirely by AI agents. This document is the honest account of the
*method* — not the code, but the org chart, the gates, and the ops that let a
team of models port a ~31k-LOC Python agent harness into Rust byte-for-byte
without the wheels coming off.

It is written to be reused. If you are pointing a fleet of agents at a large,
correctness-critical port or rewrite, this is the pattern, its evidence, and the
places it is overkill.

## The one-sentence version

**A persistent coordinator that never writes bulk code dispatches one
implementation agent per milestone, and every milestone must pass through a merge
gate whose adversarial half is designed to find exactly the bugs a green test
suite cannot.** In rho, that adversarial half found a real, shipping-blocking bug
in *every single milestone*. The table below links all five.

## The hierarchy

```text
                    ┌─────────────────────────────────────────┐
                    │            COORDINATOR                   │
                    │   (persistent, expensive frontier model) │
                    │                                          │
                    │  • runs the grill-me design interview    │
                    │  • writes the plan + milestone ladder    │
                    │  • dispatches one agent per milestone    │
                    │  • runs the MERGE GATE on every PR       │
                    │  • owns memory files + the task board    │
                    │  • hourly cron "babysit" for stalls      │
                    │                                          │
                    │  NEVER writes bulk implementation code.  │
                    └───────────────┬──────────────────────────┘
                                    │ dispatch (Agent tool, model override)
              ┌─────────────────────┼─────────────────────┐
              ▼                     ▼                     ▼
       ┌────────────┐        ┌────────────┐        ┌────────────┐
       │ MILESTONE  │        │ MILESTONE  │        │ MILESTONE  │
       │  AGENT M1  │        │  AGENT M2  │   ...  │  AGENT Mn  │
       │ (Opus-class)│       │ (Opus-class)│       │ (Opus-class)│
       │ own branch │        │ own branch │        │ own branch │
       │ opens a PR │        │ opens a PR │        │ opens a PR │
       └────────────┘        └────────────┘        └─────┬──────┘
                                                         │ (only when a milestone
                                                         │  is huge — ~6k+ LOC)
                                            ┌────────────┼────────────┐
                                            ▼            ▼            ▼
                                      ┌──────────┐ ┌──────────┐ ┌──────────┐
                                      │ CLUSTER  │ │ CLUSTER  │ │ CLUSTER  │
                                      │ subagent │ │ subagent │ │ subagent │
                                      │ worktree │ │ worktree │ │ worktree │
                                      └──────────┘ └──────────┘ └──────────┘
                                      (isolated git worktrees, merged back
                                       by the milestone agent — temporary)
```

Three tiers, but only the top two are permanent:

1. **The coordinator** is the main session, running an expensive frontier model.
   Its scarcest resource is judgment, so it spends none of it typing out
   thousands of lines of Rust. It designs, dispatches, reviews, and remembers.

2. **Milestone agents** are strong but cheaper models (Opus-class). Each gets one
   self-contained brief: scope, the exact tau reference files, a mechanical
   definition-of-done, and a git workflow. It works on its own branch, opens a
   PR, resolves the automated review-bot comments itself, and reports back.

3. **Cluster subagents** appear only when a milestone is too big for one context
   (rho's M4b was ~7k LOC, M5 ~8k). The milestone agent *becomes* a temporary
   coordinator, splits the work into independent module clusters, gives each its
   own **git worktree** on its own branch, and merges them back. This tier is
   born and dies inside a single milestone.

The shape is deliberately fractal: the same "decompose → dispatch → gate → merge"
loop runs at the top for the whole project and, when needed, one level down inside
a single milestone.

## Grill first: lock the design before any code

No milestone is dispatched until the design is settled in an adversarial
interview between the human and the coordinator. The coordinator's job in this
phase is to *refuse to start coding* and instead resolve every branch of the
decision tree: name, scope, interop guarantees, provider list, TUI strategy,
extension mechanism, testing oracle, benchmark methodology.

The output is a table of **locked decisions** that the whole plan then hangs off.
For rho that table fixed, among others: full read+write byte-compatibility with
tau's JSONL (not "similar" — byte-identical, resumable both ways), all six
providers in one milestone, ratatui for the TUI re-derived from the event stream,
WASM-via-wasmtime extensions as the final milestone, and golden fixtures extracted
from tau as the correctness oracle. Those decisions are what the milestone ladder
below is built on.

Why grill first? Because a milestone agent works from a brief, and a brief built
on an unresolved decision produces confidently-wrong code that the gate then has
to catch. Every hour of grilling removes a class of downstream rework. The
interview is where the expensive model earns its cost.

## The plan and the milestone ladder

The locked decisions become a **dependency-ordered milestone ladder**. Each rung
is sized to roughly one agent dispatch and carries its own mechanical
definition-of-done. rho's ladder:

| Milestone | Scope | DoD (mechanical) |
|---|---|---|
| M0 | Workspace scaffold + fixtures extracted from tau | fixtures committed, CI green, crosscheck skeleton runs |
| M1 | Wire types, byte-identical serde | every wire fixture parses → re-serializes byte-identical |
| M2 | Agent loop, harness, session tree, fake provider | event sequences + session files byte-identical to fixtures |
| M3 | All six provider adapters + mock SSE server | SSE fixtures → byte-identical canonical streams |
| M4a | Coding tools + print-mode CLI slice | `rho -p` vs `tau -p` byte-identical JSON event lines |
| M4b | Full `CodingSession` + CLI (split into two dispatches) | full crosscheck: identical JSONL trees + resume-swap both ways |
| M5 | ratatui TUI | ported adapter tests + insta snapshots per widget/modal |
| M6 | Benchmarks: rho vs tau | `just bench` → `dev-notes/benchmarks.md` |
| M7 | WASM extensions | examples run in TUI+print; hook parity + sandbox tests |

The ladder is the contract between coordinator and fleet. A milestone is "done"
when its DoD is *mechanically* true — not when an agent says so. That distinction
is the whole game, and it is enforced by the merge gate.

## Fixtures as the correctness oracle

Before porting a single line of behavior, M0 extracts a corpus of **golden
fixtures from tau's own serialization code**, pinned to a specific tau git
revision (`fixtures/TAU_REV`). Not hand-written expected JSON — the extraction
scripts *import tau and call the exact functions tau uses in production*
(`model_dump_json(by_alias=True, exclude_none=True)`, `entry_to_json_line`,
`render_session_html`, real provider adapters driven through a mock transport).

The policy is one sentence, and it is load-bearing:

> **If a golden test diffs, the code is wrong — never the fixture.**

A fixture a human typed encodes a human's *belief* about tau. A fixture tau
printed is ground truth. This inverts the usual review argument: nobody debates
what "correct" means, because correct is a committed file on disk. See
[`AGENTS.md`](../AGENTS.md) ("Fixture policy — read this twice") and
[`dev-notes/phase-0.md`](../dev-notes/phase-0.md).

On top of the static fixtures sits a **bidirectional crosscheck harness**
([`tools/crosscheck/`](../tools/crosscheck)): it runs identical scripted sessions
through both `tau -p` and `rho -p`, normalizes the nondeterministic bits (UUIDs →
`<id:0>`, timestamps → `<ts:0>`), and byte-diffs the results — then proves a
session written by rho *resumes in tau* and replays to the same state, and vice
versa. The oracle is not "our tests pass"; it is "the reference implementation
cannot tell rho's output apart from its own."

This matters for the gate: because fixtures and crosscheck already prove
serialization fidelity, human/adversarial review is freed to hunt exclusively for
what they *can't* catch — input tolerance, error paths, timing semantics,
resource lifecycles.

## The merge gate

Every PR, no exceptions, passes five checks before a rebase-merge:

1. **Mechanical verification, re-run independently by the coordinator.** Not
   "CI is green on the PR" — the coordinator runs `just test`, `just lint`
   (clippy `-D warnings` + `fmt --check`), and the crosscheck itself. Trust, but
   re-execute.

2. **An adversarial review by a separate, fresh agent** whose brief is: *find
   what the passing test suite cannot show.* Line-by-line fidelity against the
   tau reference; suspected divergences are verified **empirically** — run both
   implementations on the same input and diff. This agent starts cold, with no
   attachment to the code, and is told the tests already pass so it must look
   elsewhere.

3. **All automated bot threads resolved.** rho's PRs are reviewed by two public
   bots — OpenAI's Codex reviewer (`chatgpt-codex-connector`) and CodeRabbit
   (`coderabbitai`). Per [`AGENTS.md`](../AGENTS.md), every comment is either
   **fixed** (reply with the fix commit SHA) or **rebutted with evidence** (e.g.
   a `grep` over `tau/src` proving tau doesn't do the thing either). Byte-compat
   with tau is the arbiter: matching tau's *actual* behavior beats a
   plausible-sounding suggestion. Bots are often right and sometimes wrong;
   ground truth decides.

4. **A fix round** folds the confirmed findings back in.

5. **Rebase-merge**, to keep history linear and each milestone a clean band.

### The evidence: five milestones, five bugs green tests missed

This is the empirical case for the gate. In each milestone, the full test suite
was green — fixtures matched, ported tests passed — and the review layer still
found a real, shipping-blocking defect. All five are in the public PR history and
the dev-notes.

| Milestone | PR | Bug a green suite missed | Root cause | Written up |
|---|---|---|---|---|
| M1 | [#2](https://github.com/ramanshrivastava/rho/pull/2) | A persisted `usage: null` makes the **entire session file refuse to load** | An untagged enum tried to deserialize `null` into the `Usage` struct and failed the whole line; a *present* `null` is a value, not an absent field | [phase-1.md](../dev-notes/phase-1.md) |
| M2 | [#3](https://github.com/ramanshrivastava/rho/pull/3) | Harness **permanently bricked** if a consumer drops the event stream mid-run | `async-stream` runs no generator `finally` on drop, so `running` never reset and every future `prompt()` was rejected. Fixed with a `RunCleanup` RAII guard | [phase-2.md](../dev-notes/phase-2.md) |
| M3 | [#4](https://github.com/ramanshrivastava/rho/pull/4) | A total-request timeout would **kill any LLM stream** slower than the timeout | tau/httpx apply the timeout *per-read*; a naive reqwest `.timeout()` is a *total* deadline. Fixed by mapping to `read_timeout` + `connect_timeout`, no total cap | [phase-3.md](../dev-notes/phase-3.md) |
| M4a | [#5](https://github.com/ramanshrivastava/rho/pull/5) | `bash` **hangs forever** when a tool spawns a backgrounded child | The child inherits a write fd, so the reader never sees EOF and `read_to_end` blocks. Fixed with `drop(cmd)` after spawn + timeout across the whole `communicate` | [phase-4a.md](../dev-notes/phase-4a.md) |
| M4b-1 | [#6](https://github.com/ramanshrivastava/rho/pull/6) | **Silent data loss** on a persist failure | `persist_messages_since` swallowed a storage error and returned a stale count, re-appending an already-durable message. Fixed by propagating `Result` and aborting the turn | [phase-4b1.md](../dev-notes/phase-4b1.md) |

None of these is a serialization bug — fixtures already guard those. Every one is
an *error path, lifecycle, or timing* bug: precisely the class a green
byte-for-byte suite is blind to, and precisely what the adversarial brief points
at.

### The adversarial-review brief (reusable)

> You are reviewing a milestone PR whose test suite is **already green**. Assume
> the happy path works and the fixtures match — do not re-verify them. Your job
> is to find what the passing tests *cannot* show:
> - error and failure paths (what happens when persistence, the network, or a
>   subprocess fails?)
> - resource lifecycles (drops, cancellations, fds, child processes, locks)
> - timing and concurrency semantics (per-read vs total timeouts, stream
>   abandonment, re-entrancy)
> - fidelity to the reference on inputs the fixtures don't cover (tolerant
>   parsing, legacy shapes, null vs absent).
> When you suspect a divergence from the reference, **prove it**: run both
> implementations on the same input and diff. Report only what you can
> demonstrate.

## Worktree cluster parallelism

When a milestone won't fit one context, the milestone agent decomposes it into
**independent module clusters** and runs each in its own **git worktree** on its
own branch. Worktrees are the enabling trick: multiple agents edit the same repo
in parallel without touching each other's working tree, then the milestone agent
merges the branches back and runs the milestone's DoD across the union.

The rule for a clean split is *no shared mutable files between clusters* — the
crate boundaries in rho (`rho-agent` → `rho-ai` → `rho-coding` → `rho-tui`) make
this natural, since Cargo's acyclic graph already forbids the cross-edges that
would force coordination. Cluster tier is temporary: it exists for the duration
of one big milestone and then evaporates.

## Ops: keeping a fleet alive

Autonomous agents fail in boring, mechanical ways — they die on spawn, hit a
rate limit, or go idle waiting on a message that crossed with yours. The
coordinator runs a small ops layer so these don't cost a run.

- **Hourly babysit cron.** A scheduled check fires every hour: it reads the task
  board, pings each in-flight milestone agent, and detects the difference between
  an *idle heartbeat* (agent working, just quiet) and a *real stall* (agent dead
  or blocked). Stalls get a nudge or a re-dispatch.

- **Rate-limit recovery, proven in the wild.** One milestone agent died on spawn
  against a rate limit. Because the babysit cron was already scheduled, its next
  firing landed one minute after the limit reset — it noticed the dead agent and
  re-dispatched. Zero work lost, no human in the loop. The cron isn't
  belt-and-suspenders; it is the recovery mechanism.

- **Crossed-message tolerance.** The coordinator and agents message
  asynchronously, so a "still working?" and a "done, PR up" routinely cross on the
  wire. The coordinator treats messages as idempotent status, not as an
  ordered protocol — it reconciles against the task board and the actual git/PR
  state rather than trusting message ordering.

- **The teaching journal.** Every milestone writes a
  [`dev-notes/phase-N.md`](../dev-notes) entry: what was built, which Rust idiom
  replaced which Python pattern and why, and — critically — any tau behavior
  discovered that *later* milestones must respect. The journal is how knowledge
  survives the disposable-agent model: the agent that learned that
  `exclude_none` doesn't recurse into free-form JSON is gone, but the fact is
  written down where the next agent will read it.

## The mechanics underneath

The pattern is expressed with a handful of generic Claude Code team primitives;
nothing here is specific to rho:

- **The Agent tool** spawns a subagent with a model override (cheap models for
  milestones, the frontier model reserved for the coordinator) and optional
  **git-worktree isolation** for parallel clusters.
- **A message mailbox** with idle notifications lets the coordinator and agents
  talk asynchronously and wake on replies.
- **A shared task board** with `blockedBy` dependencies encodes the milestone
  ladder so the coordinator (and its cron) can see what is in-flight, done, or
  blocked.
- **Scheduled crons** run the babysit check on a fixed cadence independent of any
  live session — which is why it survives an agent death.
- **Memory files** under a per-project memory directory persist locked decisions
  and hard-won facts across sessions: small `name`/`description`/`type`-fronted
  notes indexed by a top-level `MEMORY.md`, loaded on demand.
- **Custom agent definitions** (markdown files with `name` / `description` /
  `tools` / `model` frontmatter) give recurring roles — reviewer, researcher,
  test-writer — a fixed brief you can dispatch by name.

## When to use this — and when it's overkill

This method earns its overhead only under specific conditions. Be honest about
whether you have them.

**Use it when:**

- **There is a correctness oracle you can extract up front.** A reference
  implementation, a spec with conformance fixtures, a golden corpus. The entire
  gate leans on "correct is a file on disk." Without an oracle, the adversarial
  review has nothing objective to anchor to and degrades into taste.
- **The work decomposes into dependency-ordered milestones** each sized to about
  one agent context, with mechanical done-conditions.
- **Correctness matters more than speed**, and a subtle error-path bug is
  expensive — ports, protocols, serializers, migrations, anything where "looks
  right and passes tests" is not good enough.
- **The total scope exceeds a single context** (rho is ~31k LOC of reference),
  so the coordination cost is amortized over real parallelism.

**It's overkill when:**

- The task fits in one agent's context — just dispatch one agent and review it.
- There is no external notion of correct, so tests *are* the spec and there's
  nothing for an adversarial pass to independently verify against.
- Iteration speed matters more than correctness (prototypes, spikes,
  throwaway glue) — the gate is pure tax.
- You can't run the reference and the port side by side. Empirical divergence
  checking is half the gate's value; lose it and you lose the half that catches
  the bugs green tests miss.

The honest summary: the merge gate is the expensive part and the valuable part.
If you can't feed it an oracle, most of this collapses to "dispatch agents and
read their PRs," which is fine — just don't pay for the ceremony you can't use.

## Read the source

- [`AGENTS.md`](../AGENTS.md) — fixture policy, bot-review resolution rules,
  layering constraints.
- [`dev-notes/`](../dev-notes) — the per-milestone teaching journal (each
  `phase-N.md` also writes up its review round).
- [`tools/crosscheck/`](../tools/crosscheck) — the bidirectional tau↔rho
  differential harness.
- PRs [#2](https://github.com/ramanshrivastava/rho/pull/2) –
  [#6](https://github.com/ramanshrivastava/rho/pull/6) — the merge gate in
  action, one bug per milestone, all in public view.
