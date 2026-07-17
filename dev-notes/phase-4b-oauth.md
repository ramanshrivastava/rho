# Phase 4b — credentials + OAuth cluster

Port of tau's `tau_coding/credentials.py` and `oauth*.py` into
`crates/rho-coding/src/{credentials,oauth,oauth_types,oauth_http,oauth_device,
oauth_anthropic,oauth_github_copilot,oauth_registry}.rs`.

## What was built

- `FileCredentialStore` — byte-identical `~/.rho/credentials.json`: sorted keys,
  2-space indent, `ensure_ascii` escaping, trailing newline, temp-file
  (`.credentials.json.<rand>`) + atomic rename, `chmod 0600`. `set`/`set_api_key`
  persist a **bare string** (tau's default), not a `{"type":"api_key"}` object;
  the object form only arises when loading one another tool wrote.
- `OAuthCredential` / `ApiKeyCredential` with tau's exact `to_json` field order
  (`type, access, refresh, expires, [account_id], [metadata]`). The file dump
  re-sorts keys anyway, so order matters only for direct map readers.
- OpenAI Codex, Anthropic, GitHub Copilot providers behind the `OAuthProvider`
  trait (`id`/`name`/`flow_kinds`/`refresh`/`runtime_auth`); a process-global
  registry mirroring tau's mutable module dict (insertion order preserved).
- Device-code polling with the RFC 8628 timing + cancellation, `sleep`/`monotonic`
  injectable exactly as tau.

## Seams introduced (rho-specific, no behavior change)

- **Time**: `oauth_credential_is_expired(cred, now_ms)` and every refresh fn take
  an explicit `now_ms: i64` (integer ms, the granularity of
  `rho_agent::clock::Clock::now_ms`). Production passes `clock.now_ms()`; tests
  pin it. This replaces tau's ambient `time.time()`.
- **HTTP**: token calls go through the `OAuthHttpClient` trait. Production is
  `ReqwestOAuthClient` (wraps the shared `rho_ai::http` client); tests inject
  `MockHttpClient` — the faithful analog of tau's `httpx.MockTransport(handler)`.
  **No test hits the network.** reqwest has no transport hook, so this seam is the
  only way to reproduce tau's mock-transport tests.

## Parity deviations journaled

1. **`OAuthProvider::refresh` signature.** tau's `refresh(credential)` constructs
   its own httpx client and reads `time.time()`. rho threads the client + `now_ms`
   as parameters (`refresh(cred, client, now_ms)`) so the integrator supplies a
   shared client + clock and tests stay deterministic. Behavior is identical.
2. **Error prose keeps tau's literal "Tau …".** e.g. `"Tau credentials must be a
   JSON object"`. The task requires validation messages verbatim, and this
   matches the existing rho precedent of keeping tau's exact user-facing strings
   (`system_prompt` still says "operating inside Tau"). Wire values likewise stay
   `tau` (`user-agent: claude-cli/tau`, `originator=tau`) for provider byte-compat.
3. **`"Tau credential names must be strings"` is unreachable in Rust.** JSON object
   keys are always `String` under serde, so this branch can't fire; kept
   conceptually via the typed map. No behavioral difference.
4. **Device-poll network error mapping.** If `OAuthHttpClient::send` errors mid
   device-poll, rho surfaces it as `DevicePollResult::failed(msg)` → `OAuthError`,
   whereas tau would propagate the raw httpx exception. Both are errors; the mock
   never errors so no tested path differs.
5. **Millisecond expiry from float responses.** `expires_in`/`expires_at`/JWT
   `exp` are computed with integer arithmetic when the JSON number is an integer
   (exact, matches Python `int(x*1000)`); only genuinely-float inputs use an
   `f64` cast + truncation. Truncation is intentional (`clippy::cast_*` allowed
   with rationale), matching Python's `int()`.

## Not ported / skipped

- `test_oauth_tui.py` — TUI, out of cluster.
- `test_runtime_oauth_resolver_refreshes_and_persists_atomically` — exercises
  `provider_runtime.OAuthRuntimeCredentialResolver` + `provider_config`, which live
  in the provider_runtime cluster (out of scope here). The registry
  register/unregister/reset and `set_oauth` atomic-write pieces it leans on **are**
  covered by our own tests.
- Interactive browser/device end-to-end logins — see
  `dev-notes/oauth-manual-checklist.md`.
