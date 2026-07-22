# Identity vs parity: how rho introduces itself as rho

rho is a byte-compat port of tau. To prove that compatibility, several rho
surfaces are golden-matched to tau's exact bytes — most notably the assembled
system prompt (`fixtures/system-prompt/default_coding_tools.txt`) and the HTML
session export (`fixtures/export/kitchen-sink.html`). Both were extracted from
tau, so they carry tau's *name* in their chrome:

- the system prompt opens `"You are an expert coding assistant operating inside
  Tau, a coding agent harness."`
- the HTML export chrome shows `Tau session export` and stores the theme under a
  `tau-session-export-theme` key.

When rho golden-matched those bytes it inherited tau's identity, so a real user
running rho was told they were "operating inside Tau" (user-reported bug).

## The seam

The fix is a single, explicit **brand seam** in each assembly path:

| Surface | Assembly | Real default | Parity override |
|---|---|---|---|
| System prompt | `BuildSystemPromptOptions.brand` (`system_prompt.rs`) | `"rho"` | golden test passes `Some("Tau")` |
| HTML export | `render_html(.., brand)` (`session_export.rs`, via `EXPORT_BRAND`) | `"Rho"` | golden test passes `"Tau"` |

The static template constants (`HTML_2`/`HTML_8`, the prompt format string) keep
tau's tokens, so the parity override is a **no-op** — the byte-for-byte golden is
reproduced verbatim. Only the production default swaps the harness name. This is
the one sanctioned place where "brand = Tau ⇒ byte-identical to tau" is asserted;
everywhere else rho is rho.

**Never regenerate or edit the fixtures.** They are tau's oracle. The seam exists
precisely so we can keep them pristine while shipping rho's own identity.

## Everything else is a plain string fix

The rest of rho's user-facing identity was leaked as plain literals, not through
a parity surface, so they were simply rebranded (no seam needed):

- session/export titles: `Rho session <id>` / `Rho Session Export`
  (`session.rs`, `main.rs`, `DEFAULT_EXPORT_TITLE`)
- OAuth callback page `<title>Rho OAuth</title>` (`oauth.rs`)
- credential-file validation errors `"Rho ..."` (`credentials.rs`) — the on-disk
  byte-parity covers the credential *file format*, not the error prose
- the Codex reasoning-effort explanation (`provider_config.rs`)

## What stays "tau" on purpose

These are **not** identity leaks and must not change:

- **Wire / transport identifiers** that impersonate tau's client to providers for
  API and telemetry compatibility: the Anthropic `claude-cli/tau` user-agent
  (`oauth_anthropic.rs`), the OpenAI Codex `originator: "tau"` (`env.rs`) and its
  `tau (…)` user-agent (`openai_codex.rs`).
- **Fixtures, crosscheck expected files, and parity test assertions** — the
  oracle side of the differential.
- **References to tau-the-project** in doc comments and dev-notes.
- Test scaffolding system prompts (`"You are Tau."`) and `.tau` paths used only in
  tests. (rho's real home is `~/.rho`; see `paths.rs`.)

One borderline case left as-is: `IGNORED_FILE_COMPLETION_DIRS` in
`autocomplete.rs` still lists `.tau` (tau's completion-ignore constant). It is
not user-visible identity; adjusting it to `.rho` is a separate behavior change.

# Sync-round ledger: tier-3 polish batch

This branch (`sync/tau-polish-tier3`) tracks tau's tier-3 UI-polish batch. Each
upstream commit is recorded as **ported**, **N/A** (rho lacks the surface tau
changed), or **deliberate non-port** (feature intentionally not carried).

## Ported

| tau commit | What | rho port |
|---|---|---|
| `fd327d0` | Remove redundant tool-row spinner | Dropped `TOOL_SPINNER_FRAMES` / `apply_tool_spinner` / `tool_spinner` state; a pending tool row keeps a static tool-border marker and the whole-second elapsed timer (`state.rs`, `transcript.rs`, `app.rs`, `composer.rs`). |
| `f025e1d` | Light-theme code-block background | rho already applies `markdown_code_block_background` directly (no Textual CSS to strip); added cross-theme regression coverage (`transcript.rs`). |
| `b2745d8` | Picker highlight contrast | rho already paints selected rows with the themed `completion_selected` fg+bg; added all-built-in-theme regression coverage (`modals.rs`). |
| `2027b8c` | Tool-call labels in session tree | `tree_entry_title` now gates on visible text (`a.text().trim().is_empty()`) rather than `content.is_empty()`, so tool-only assistant turns are labeled `tool call: …` (`session.rs`, `coding_session.rs` tests). See N/A note for the app.py half. |
| `102482b` | Custom-prompt auto-naming order | Auto-naming is deferred until *after* the user `MessageEnd` event is yielded, so the expanded prompt renders before the naming provider request (`session.rs`, `coding_session.rs` tests). |
| `e3fc26d` | Shorten home context paths in sidebar | New `context_file_label` (cwd-relative for project files, `~/`-abbreviated under `$HOME`, absolute otherwise); wired into `build_chrome` (`status.rs`, `mod.rs`, `app.rs`). |
| `7f4be2c` | Clarify token usage | Sidebar totals relabeled `cumulative usage`, distinct from the footer's active-context ratio (`sidebar.rs`, sidebar snapshot). |
| `dd49d9d` | **Default the TUI sidebar to the right** (owner-requested) | `SidebarPosition::default()` → `Right`; empty-config default → `right`; `app.rs` layout honors `Off`/`Left`/`Right` (right column when `Right`, hidden when `Off`/too-narrow) (`theme.rs`, `app.rs`, tests). This is a **port at owner request**, not a non-port. |

## N/A — rho lacks the surface tau changed

- **`e5eb252` (style footer provider as metadata).** tau's compact footer is a
  multi-tone Rich `Text` where the change dims the provider name to
  `completion_description` while the model stays the brighter `prompt_text`. rho
  renders the compact-footer right column as a *single* line uniformly styled
  `muted_text` (`fit_two_columns` in `status.rs`), a deliberate simplification of
  tau's two-row layout. In every rho built-in theme `completion_description ==
  muted_text`, so the provider is *already* shown in exactly the metadata tone
  this commit introduces, and rho does not emphasize the model, so there is no
  brighter model text for the provider to be dimmed relative to. Porting would
  mean *adding* model emphasis plus multi-span truncation — out of scope for a
  color tweak.
- **`2027b8c` app.py half (recolor highlighted tree-picker author span).** tau
  recolors the accent-colored `author:` span to `highlight_text` when a tree row
  is highlighted, because Textual's highlight background clashes with the accent.
  rho's tree-picker rows are single-style via `list_row`, and the selected row
  already uses the high-contrast `completion_selected` fg+bg (guaranteed by the
  `b2745d8` coverage). There is no per-span author accent to become unreadable,
  so tau's `watch_highlighted` recolor has no analog. The session.py half of
  `2027b8c` *is* ported (see table above).

## Deliberate non-ports (this round)

- **tau `48afd91` + `b74e0dd` — "Tau self-knowledge" skill bundle.** Ships a
  bundled skill teaching the agent about *tau itself* (and `b74e0dd` keeps it out
  of user skills). This is tau-project identity content, not a parity surface;
  rho does not carry tau's self-knowledge bundle.
- **Release chores / version bumps.** Upstream changelog, website, and release
  housekeeping commits carry no rho behavior.
- **Test-only upstream churn** — `1b74b23` (isolate env + set-equality in
  `test_provider_config`), the *test* portion of `1ca6348` (file-drop tests; the
  feature itself already landed in rho via PR #26), and `e3b7d2d` (de-flake a
  login-search focus assertion). These touch tau's Python test suite only; rho's
  equivalent behavior is already covered by its own Rust tests.

## Deferred (tracked follow-up)

- **tau `ed0dde0` — data-driven themes with discovery.** Deferred; too entangled
  with rho's `Copy` `TuiThemeName` enum and the `rho-coding` ⇄ `rho-tui` layering
  to port cleanly in a polish batch. Full architectural rationale and the
  recommended refactor-first follow-up are in the PR #26 body ("Data-driven
  themes with discovery").
