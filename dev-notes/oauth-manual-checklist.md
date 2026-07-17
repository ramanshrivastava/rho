# OAuth interactive-flow manual verification checklist

The credentials + OAuth cluster (`crates/rho-coding/src/credentials.rs`,
`oauth*.rs`) is a byte-compat port of tau's `tau_coding/credentials.py` and
`oauth*.py`. Every **non-interactive** path is unit-tested:

- credential file round-trips + byte-format (`credentials::tests`)
- authorization-URL construction, PKCE, `parse_authorization_input`,
  `oauth_credential_is_expired`, `account_id_from_access_token` (`oauth::tests`)
- OpenAI Codex token refresh (mock HTTP) (`oauth::tests`)
- Anthropic token refresh + failure redaction (mock HTTP) (`oauth_anthropic::tests`)
- GitHub Copilot **device login** end-to-end + enterprise refresh + untrusted-URI
  rejection (mock HTTP) (`oauth_github_copilot::tests`)
- device-code polling: `slow_down` back-off + cancellation (`oauth_device::tests`)
- provider registry (`oauth_registry::tests`)

The network is **never** touched: token calls go through the
`OAuthHttpClient` trait, and tests inject `MockHttpClient` (the analog of tau's
`httpx.MockTransport(handler)`).

## What is NOT unit-tested (interactive / requires real IdP + browser + sockets)

These functions open a browser, bind a local TCP callback server, and/or complete
a real OAuth handshake with the provider. They cannot run in CI. Verify manually
after any change to `oauth.rs` / `oauth_anthropic.rs` server or login code.

| Flow | Entry point | Why manual |
|------|-------------|-----------|
| OpenAI Codex browser login | `oauth::login_openai_codex` | opens browser, binds `127.0.0.1:1455`, exchanges a real auth code |
| Anthropic browser login | `oauth_anthropic::login_anthropic` | opens browser, binds `127.0.0.1:53692`, real token exchange |
| Local callback server | `oauth::start_local_oauth_server` / `LocalOAuthServer` | threaded blocking `TcpListener`, real HTTP redirect |
| Browser open | `oauth::open_url` | spawns `open`/`xdg-open`/`cmd start` |
| GitHub Copilot device login against **real** github.com | `oauth_github_copilot::login_github_copilot` | the flow *logic* is unit-tested with a mock; a real end-to-end run still needs a browser + GitHub account |

### Manual: OpenAI Codex (ChatGPT subscription)

1. Wire an `OpenAICodexOAuthProvider` and call `login_openai_codex(callbacks,
   &ReqwestOAuthClient::default(), clock.now_ms(), true, "tau")`.
2. A browser opens `https://auth.openai.com/oauth/authorize?...`. Complete login.
3. The local server on `:1455` receives `/auth/callback?code=...&state=...`,
   shows the "OpenAI authentication completed" page, and the code is exchanged.
4. Expect an `OAuthCredential` with a non-empty `access`/`refresh`, `expires` in
   the future, and `account_id` extracted from the access JWT.
5. Persist via `FileCredentialStore::set_oauth("openai-codex", cred)` and confirm
   `~/.rho/credentials.json` is mode `0600` with sorted keys + trailing newline.
6. "Browser on another machine" branch: decline the auto-open, paste the full
   redirect URL at the prompt; `state` mismatch must raise `OAuth state mismatch`.

### Manual: Anthropic (Claude Pro/Max)

1. Call `login_anthropic(callbacks, &ReqwestOAuthClient::default(),
   clock.now_ms(), true)`.
2. Browser opens `https://claude.ai/oauth/authorize?...`; complete login.
3. Local server on `:53692` receives `/callback?code=...`; token exchange runs
   against `https://platform.claude.com/v1/oauth/token`.
4. Expect a credential with `account_id == None` and `expires` ≈ `now +
   expires_in - 5min` skew. `runtime_auth` must emit the four headers
   (`Authorization: Bearer …`, `anthropic-beta`, `user-agent: claude-cli/tau`,
   `x-app: cli`) in that order.

### Manual: GitHub Copilot device flow (real end-to-end)

1. Call `login_github_copilot(callbacks, &ReqwestOAuthClient::default(), None,
   clock.now_ms())`. At the prompt, press enter for github.com (or type an
   Enterprise domain).
2. `on_device_code` fires with a `user_code` + `verification_uri`; open it, enter
   the code, authorize.
3. Polling honors the server `interval`/`slow_down`; on success the long-lived
   GitHub token is exchanged for a short-lived Copilot token.
4. `runtime_auth` `base_url` must resolve from the `proxy-ep` field of the Copilot
   token (or `copilot-api.<enterprise>` for Enterprise).

### Environment knobs

- `TAU_OAUTH_CALLBACK_HOST` overrides the callback bind host (default `127.0.0.1`),
  same variable name as tau (the OAuth callback contract is provider-facing).
