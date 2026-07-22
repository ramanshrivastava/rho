# Changelog

All notable changes to rho are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-07-22

### Changed
- Splash brand refresh: the name row is now `ρω · r h o · रो` (bold Latin
  wordmark center); the benchmark brag is three aligned branch-marker bullets
  (cold start, weight, stream canonicalization), all computed from the
  committed benchmark records (#31).
- README beautified: badges, hero splash, headline benchmark table, install
  up top; MIT LICENSE file added carrying upstream tau/pi notices (#32).

### Fixed
- Ghost default-background cell after the Devanagari name in the splash:
  rho now pins the terminal default background to the theme background via
  OSC 11 (reset via OSC 111 on exit), fixing the class of cells ratatui's
  diff can never emit for width-disagreeing clusters (#33).

## [0.1.0] - 2026-07-22

First public release: a complete, byte-compatible Rust port of
[tau](https://github.com/huggingface/tau). A session started in tau resumes
end-to-end in rho and vice versa, enforced by golden-fixture parity in CI.

### Added

- **Full tau port, M0–M7.** Wire types with byte-identical serde (M1); agent
  loop, harness, and session tree (M2); all six providers over raw HTTP/SSE —
  `anthropic`, `openai-compatible`, `codex`, `google`, `mistral`, `fake` (M3);
  the coding tools, print-mode CLI, and full `CodingSession` (M4); the ratatui
  TUI at parity with tau's Textual TUI (M5); the rho-vs-tau-vs-pi benchmark
  suite (M6); and the wasmtime WASM extension host + guest API (M7).
- **Subscription OAuth sign-in** via `/login [provider]`: OpenAI Codex (ChatGPT)
  and Anthropic (Claude Pro/Max) browser authorization-code + PKCE flows, and
  GitHub Copilot device-code flow. Credentials persist to
  `~/.rho/credentials.json` in tau's exact on-disk format and refresh
  automatically; `/logout` removes them.
- **TUI identity features:** the rho welcome splash (multi-script name, animated
  π → τ → ρ lineage, real benchmark brag), bottom-anchored transcript, a
  blinking cursor, and scrollback.

### Fixed

- OAuth interactive-flow fixes — local callback server, state handling, and
  browser-open behavior (#23).
- Codex provider: duplicate `Content-Type` header on token/request calls (#24).
- tau sync round porting upstream parity fixes and TUI features: parity-critical
  serialization fixes and `TAU_REV` bump (#25), session insights, resume search,
  theme discovery, and error recovery (#26), and a polish batch — spinner,
  contrast, labels, footer, home-path shortening, and sidebar-right layout (#27).

[Unreleased]: https://github.com/ramanshrivastava/rho/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ramanshrivastava/rho/releases/tag/v0.1.0
