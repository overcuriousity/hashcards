# OIDC Authentication and Multi-User Collections — Design

Status: approved 2026-09-01.

Written before the server-only refactor
([2026-09-02](2026-09-02-server-only-refactor-design.md)), so the CLI
surface below is stale: there is no `serve`/`drill` subcommand pair and no
directory-args mode any more. One binary, `hashcards-web [--config <path>]`,
and a config file is mandatory. The auth design itself shipped as written.

**Scope:** the server.

## Goal

Add optional OIDC login to `serve` (validated in practice against a
Nextcloud OIDC provider) so a single hashcards instance can host multiple
users, each seeing only their own collections. No sharing between users, no
self-service signup or account UI — a user is provisioned by an admin
editing the TOML config and restarting the server.

When `[oidc]` is absent from the config, the server's behavior is byte-for
byte unchanged from today: no login, all configured collections open to
anyone who can reach the port, exactly as documented in the current
README. This is a strict opt-in.

## Config changes

### New `[oidc]` section (`ServeConfig`)

```toml
[oidc]
issuer_url = "https://cloud.example.com/index.php/apps/oidc"
client_id = "..."
client_secret = "..."
external_url = "https://hashcards.example.com"
session_secret = "..."
# Optional, default shown:
# scopes = ["openid", "email", "profile"]
```

- `issuer_url`: OIDC discovery base; `{issuer_url}/.well-known/openid-configuration`
  is fetched at startup. Startup fails with a clear error if discovery fails
  or the document lacks required endpoints/claims support.
- `client_id` / `client_secret`: standard OAuth2 client credentials
  registered with the IdP (in Nextcloud: Administration → Security →
  OIDC/OAuth2 client).
- `external_url`: the public base URL users reach the server through. Needed
  to build a correct `redirect_uri` (`{external_url}/auth/callback`) when
  `host`/`port` bind to `127.0.0.1` behind a reverse proxy — `host`/`port`
  are irrelevant to what the browser and IdP see.
- `session_secret`: a long random string the admin generates once (like
  `client_secret`) and keeps stable across restarts. Signs the session
  cookie. Rotating it logs out every user.
- `scopes`: defaults to `["openid", "email", "profile"]`. Config load fails
  if `openid` or `email` is missing — the `email` claim is required for
  ownership matching.

Parsing/resolution mirrors the existing `[git]` section's `Option<...>`
pattern in `ServeConfig`/`ResolvedServeConfig`.

### `owner` field on collections and HedgeDoc entries

```rust
pub struct CollectionEntry {
    pub name: String,
    pub path: String,
    pub owner: Option<String>,   // new
}

pub struct HedgedocEntry {
    pub url: String,
    pub owner: Option<String>,   // new
}
```

`owner` is an email, matched case-insensitively against the OIDC `email`
claim. `ResolvedCollection` gains the same field (lowercased at resolution
time, once, so every comparison downstream is a plain `==`).

**Validation, only when `[oidc]` is present:** `ResolvedServeConfig::from_toml`
fails with a clear message (naming the offending collection/note) if any
`CollectionEntry` or `HedgedocEntry` lacks an `owner`. When `[oidc]` is
absent, `owner` is inert — present or absent, it changes nothing (so a
config can be prepared with `owner` fields ahead of turning on `[oidc]`).

Directory-args mode (`hashcards serve DIR1 DIR2`, no config file) cannot
express `owner` or `[oidc]` at all — same limitation directory-args mode
already has for `[git]`/`[hedgedoc]`. Attempting `--host 0.0.0.0` style
exposure without a config file remains an unauthenticated, single-user
setup, as today.

Slug uniqueness stays global (existing `check_slug_collisions`, unchanged)
— two users cannot pick colliding slugs even though they can't see each
other's collections, which keeps the DB-path derivation untouched.

## Session mechanism

Stateless, signed cookies via `axum-extra`'s `SignedCookieJar` (built on the
`cookie` crate's HMAC-SHA256 signing) rather than a hand-rolled MAC — no new
crypto code to review, and the cookie holds no secret data so signing
(integrity) is sufficient; encryption is not needed.

Two cookies, both `HttpOnly`, `SameSite=Lax`, `Secure` when `external_url`
is `https://`:

- `hc_session`: `{ email: String, expires_at: Timestamp }`, set on
  successful callback, cleared on `/auth/logout`. Lifetime: 30 days,
  re-issued on each authenticated request within the last 7 days of
  validity (sliding window) so an active user is never logged out
  mid-session.
- `hc_oidc_flow`: `{ csrf_token, nonce, pkce_verifier, return_to }`, set by
  `/auth/login`, consumed and cleared by `/auth/callback`. 10-minute
  lifetime. Carries the in-flight login state so no server-side store is
  needed for it either.

Both are signed with the config's `session_secret` (the `cookie` crate's
`Key`, derived from the configured string via its own KDF).

## New routes and middleware

Added only when `[oidc]` is configured (checked once at router-build time
in `src/cmd/serve/server.rs`):

- `GET /auth/login` — builds the IdP authorize URL via `openidconnect`
  (PKCE + nonce + CSRF token generated by the crate, not by us), stashes
  them plus `return_to` (from `?return_to=`, defaulting to `/`) in
  `hc_oidc_flow`, redirects to the IdP.
- `GET /auth/callback?code=...&state=...` — validates `state` against the
  stashed CSRF token, exchanges `code` for tokens, validates the ID token
  (signature, `nonce`, `aud`, `exp`) via `openidconnect`, reads the `email`
  claim, sets `hc_session`, clears `hc_oidc_flow`, redirects to `return_to`.
  Any failure (state mismatch, expired flow cookie, invalid token) renders
  a plain error page and does **not** create a session.
- `GET /auth/logout` — clears `hc_session`; if the discovery document
  advertises `end_session_endpoint`, redirects there
  (`post_logout_redirect_uri = external_url`), else redirects to `/`.
- `require_auth` middleware (`axum::middleware::from_fn_with_state`),
  layered over every route except `/auth/*`: reads `hc_session`; valid and
  unexpired → inserts a `CurrentUser { email: String }` request extension
  and proceeds; missing/invalid/expired → redirects to
  `/auth/login?return_to={original path+query}`.

When `[oidc]` is absent, none of the above is registered and no middleware
layer is added — the router is built exactly as it is today.

## Authorization

`find_collection` (`src/cmd/serve/handlers.rs:215`) is the single place
almost every handler resolves a slug through. It gains an `owner: Option<&str>`
parameter (`None` when `[oidc]` is off):

```rust
pub(super) fn find_collection(
    state: &AppState,
    slug: &str,
    owner: Option<&str>,
) -> Option<ResolvedCollection>
```

It filters both the static-collections branch and the HedgeDoc-sources
branch by `rc.owner.as_deref() == owner` before matching on slug — when
`owner` is `None` (auth off) every collection matches, preserving today's
behavior exactly. Every call site pulls `owner` from the `CurrentUser`
extension (via an extractor) when present.

A slug that exists but belongs to someone else is indistinguishable from a
slug that doesn't exist: both fall through to the existing "unknown
collection" 404, so users can't enumerate each other's collections by
guessing.

The landing page (`landing_handler`, `build_combined_infos`) filters
`CollectionInfo` the same way before rendering, so a user only ever sees
their own collections and HedgeDoc-backed decks in the list, counts, and
due-today totals. `CollectionInfo` gains an `owner: Option<String>` field
carried through from `ResolvedCollection`/`HedgedocSource`.

A logged-in user whose email matches no `owner` anywhere sees an empty
landing page — not an error. This is the expected steady state right after
an admin adds `[oidc]` but before editing in that user's collections.

## Git commit authorship (edit-and-commit)

`edit_post_inner` (`src/cmd/serve/edit.rs:277`) currently always uses the
configured `[git]` `commit_author_name`/`commit_author_email` (or a fixed
fallback). When `[oidc]` is on, it uses the current user's email as both
git author name and email instead — better audit trail in git history for
who edited what. Falls back to today's behavior when `[oidc]` is off. This
only changes commit metadata, not the commit/edit logic itself.

## New module and dependencies

- New `src/cmd/serve/auth.rs`: `OidcState` (discovery client, built once at
  startup and stored in `AppState` as `Option<Arc<OidcState>>`), the three
  route handlers, the `require_auth` middleware, the `CurrentUser`
  extractor, and the two cookie payload types.
- `Cargo.toml` additions: `openidconnect` (OIDC/OAuth2 client — discovery,
  PKCE, nonce, ID-token validation), `axum-extra` with the `cookie-signed`
  feature.

## Testing

- **Back-compat regression:** every existing `serve` integration test
  continues to pass unmodified with no `[oidc]` section — proves the
  opt-in is truly inert by default.
- **Config validation:** loading an `[oidc]`-enabled config with a
  collection or HedgeDoc entry missing `owner` fails with a message naming
  that entry. An `[oidc]`-enabled config with `owner` set on everything
  loads cleanly.
- **Full login round trip:** a small mock OIDC provider stood up in-test
  (an axum server on a local port serving `.well-known/openid-configuration`,
  JWKS, and a token endpoint, signing test ID tokens with a throwaway
  RSA/EC key generated at test start) drives `/auth/login` →
  IdP → `/auth/callback` → an authenticated request to a `owner`-matching
  collection, and asserts the session cookie is set and the request
  succeeds.
- **Cross-user isolation:** with two collections owned by two different
  emails, a session for user A requesting user B's slug gets 404, on every
  handler that takes a slug (get, post, start, stats, bookmarks, edit,
  file, script).
- **Logout:** clears the cookie; a subsequent request redirects to login.
- **Sliding session renewal:** a request within the renewal window
  refreshes `hc_session`'s expiry.

## Out of scope (explicitly)

- Self-service account creation, invites, or any admin UI for user
  management — config-file editing only.
- Sharing a collection between users.
- `drill` mode authentication.
- Server-side session revocation/"log out everywhere" (accepted trade-off
  of the stateless-cookie choice; rotating `session_secret` is the blunt
  equivalent).
