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
