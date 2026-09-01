use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::response::Response;
use axum_extra::extract::cookie::Cookie;
use axum_extra::extract::cookie::SameSite;
use axum_extra::extract::cookie::SignedCookieJar;
use serde::Deserialize;
use serde::Serialize;
use time::Duration as CookieDuration;

use crate::cmd::serve::config::ResolvedOidc;
use crate::cmd::serve::state::AppState;
use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::types::timestamp::Timestamp;

const SESSION_COOKIE: &str = "hc_session";
const FLOW_COOKIE: &str = "hc_oidc_flow";
/// Sessions are valid for 30 days from issue, and re-issued (sliding) once
/// fewer than 7 days remain, so an active user is never logged out
/// mid-session.
const SESSION_LIFETIME_MINUTES: i64 = 30 * 24 * 60;
const SESSION_RENEW_WITHIN_MINUTES: i64 = 7 * 24 * 60;
const FLOW_LIFETIME_MINUTES: i64 = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentUser {
    pub email: String,
}

#[derive(Serialize, Deserialize)]
struct SessionPayload {
    email: String,
    /// `Timestamp` only implements `Serialize` (see `types::timestamp`), so
    /// the expiry is round-tripped through its string form.
    expires_at: String,
}

/// `secure` marks the cookie `Secure`, so it is only ever sent over HTTPS.
/// It is derived from the deployment's `external_url` scheme rather than
/// hardcoded, because a local HTTP deployment could not log in at all with
/// `Secure` always set.
pub(super) fn set_session_cookie(
    jar: SignedCookieJar,
    email: &str,
    secure: bool,
) -> SignedCookieJar {
    let expires_at = Timestamp::now().minus_minutes(-SESSION_LIFETIME_MINUTES);
    let payload = SessionPayload {
        email: email.to_string(),
        expires_at: expires_at.to_string(),
    };
    // Serialization of a struct built above cannot fail.
    let value = serde_json::to_string(&payload).unwrap_or_default();
    let mut cookie = Cookie::new(SESSION_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_path("/");
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    // Without an explicit lifetime this is a browser-session cookie, which
    // would log the user out on every browser restart no matter what the
    // payload says. Both are derived from `SESSION_LIFETIME_MINUTES` at the
    // same instant, so the cookie and its payload expire together.
    cookie.set_max_age(CookieDuration::minutes(SESSION_LIFETIME_MINUTES));
    jar.add(cookie)
}

pub(super) fn read_session(jar: &SignedCookieJar) -> Option<CurrentUser> {
    let cookie = jar.get(SESSION_COOKIE)?;
    let payload: SessionPayload = serde_json::from_str(cookie.value()).ok()?;
    let expires_at = Timestamp::try_from(payload.expires_at).ok()?;
    if expires_at.into_inner() < Timestamp::now().into_inner() {
        return None;
    }
    Some(CurrentUser {
        email: payload.email,
    })
}

/// True when a valid session has fewer than `SESSION_RENEW_WITHIN_MINUTES`
/// remaining and should be re-issued with a fresh expiry.
pub(super) fn session_needs_renewal(jar: &SignedCookieJar) -> bool {
    let Some(cookie) = jar.get(SESSION_COOKIE) else {
        return false;
    };
    let Ok(payload) = serde_json::from_str::<SessionPayload>(cookie.value()) else {
        return false;
    };
    let Ok(expires_at) = Timestamp::try_from(payload.expires_at) else {
        return false;
    };
    let renew_threshold = Timestamp::now().minus_minutes(-SESSION_RENEW_WITHIN_MINUTES);
    expires_at.into_inner() < renew_threshold.into_inner()
}

pub(super) fn clear_session_cookie(jar: SignedCookieJar) -> SignedCookieJar {
    // The removal cookie must carry the same `Path` as the one that was set,
    // or the browser keeps the original.
    let mut cookie = Cookie::from(SESSION_COOKIE);
    cookie.set_path("/");
    jar.remove(cookie)
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct OidcFlowState {
    pub csrf_token: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub return_to: String,
}

#[derive(Serialize, Deserialize)]
struct FlowPayload {
    state: OidcFlowState,
    expires_at: String,
}

pub(super) fn set_flow_cookie(
    jar: SignedCookieJar,
    state: &OidcFlowState,
    secure: bool,
) -> SignedCookieJar {
    let expires_at = Timestamp::now().minus_minutes(-FLOW_LIFETIME_MINUTES);
    let payload = FlowPayload {
        state: state.clone(),
        expires_at: expires_at.to_string(),
    };
    let value = serde_json::to_string(&payload).unwrap_or_default();
    let mut cookie = Cookie::new(FLOW_COOKIE, value);
    cookie.set_http_only(true);
    cookie.set_path("/");
    cookie.set_same_site(SameSite::Lax);
    cookie.set_secure(secure);
    cookie.set_max_age(CookieDuration::minutes(FLOW_LIFETIME_MINUTES));
    jar.add(cookie)
}

/// Consumes the flow cookie: returns the jar with it removed, and the state
/// it held, if present and unexpired.
pub(super) fn take_flow_cookie(jar: SignedCookieJar) -> Option<(SignedCookieJar, OidcFlowState)> {
    let cookie = jar.get(FLOW_COOKIE)?;
    let payload: FlowPayload = serde_json::from_str(cookie.value()).ok()?;
    let jar = jar.remove(Cookie::from(FLOW_COOKIE));
    let expires_at = Timestamp::try_from(payload.expires_at).ok()?;
    if expires_at.into_inner() < Timestamp::now().into_inner() {
        return None;
    }
    Some((jar, payload.state))
}

/// Discovery (`CoreClient::from_provider_metadata`) always sets the
/// authorization endpoint and may set the token/userinfo endpoints
/// (guaranteed present by the OIDC spec, but typed as "maybe" by the
/// crate); the type-state parameters below must match exactly what that
/// call produces, or the type simply won't match.
type DiscoveredCoreClient = openidconnect::core::CoreClient<
    openidconnect::EndpointSet,
    openidconnect::EndpointNotSet,
    openidconnect::EndpointNotSet,
    openidconnect::EndpointNotSet,
    openidconnect::EndpointMaybeSet,
    openidconnect::EndpointMaybeSet,
>;

pub struct OidcRuntime {
    client: DiscoveredCoreClient,
    scopes: Vec<openidconnect::Scope>,
    /// True when `external_url` is HTTPS, in which case the session and flow
    /// cookies are marked `Secure`.
    secure_cookies: bool,
}

impl OidcRuntime {
    pub(super) fn secure_cookies(&self) -> bool {
        self.secure_cookies
    }
}

/// A `return_to` target is only honoured when it is a path on this server.
/// It must start with a single `/`; `//host` and `/\host` are read by
/// browsers as protocol-relative URLs to another origin, and control
/// characters cannot go in a `Location` header at all. Anything else falls
/// back to the site root, so a crafted login link cannot bounce a user to an
/// attacker's page immediately after a genuine login.
fn safe_return_to(raw: &str) -> String {
    let local = raw.starts_with('/')
        && !raw.starts_with("//")
        && !raw.starts_with("/\\")
        && !raw.chars().any(|c| c.is_control());
    if local {
        raw.to_string()
    } else {
        "/".to_string()
    }
}

pub(super) async fn build_oidc_runtime(config: &ResolvedOidc) -> Fallible<OidcRuntime> {
    use openidconnect::ClientId;
    use openidconnect::ClientSecret;
    use openidconnect::IssuerUrl;
    use openidconnect::RedirectUrl;
    use openidconnect::Scope;
    use openidconnect::core::CoreClient;
    use openidconnect::core::CoreProviderMetadata;

    // Redirects disabled: discovery/token requests should not follow
    // redirects to third-party hosts (SSRF hardening), matching current
    // openidconnect guidance for the caller-supplied HTTP client.
    let http_client = openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| ErrorReport::new(format!("failed to build OIDC HTTP client: {e}")))?;

    let issuer_url = IssuerUrl::new(config.issuer_url.clone())
        .map_err(|e| ErrorReport::new(format!("invalid [oidc].issuer_url: {e}")))?;

    let metadata = CoreProviderMetadata::discover_async(issuer_url, &http_client)
        .await
        .map_err(|e| {
            ErrorReport::new(format!(
                "failed to discover OIDC provider metadata at [oidc].issuer_url: {e}"
            ))
        })?;

    let redirect_url = RedirectUrl::new(format!(
        "{}/auth/callback",
        config.external_url.trim_end_matches('/')
    ))
    .map_err(|e| ErrorReport::new(format!("invalid [oidc].external_url: {e}")))?;

    let client = CoreClient::from_provider_metadata(
        metadata,
        ClientId::new(config.client_id.clone()),
        Some(ClientSecret::new(config.client_secret.clone())),
    )
    .set_redirect_uri(redirect_url);

    let scopes = config.scopes.iter().cloned().map(Scope::new).collect();
    let secure_cookies = config.external_url.starts_with("https://");

    Ok(OidcRuntime {
        client,
        scopes,
        secure_cookies,
    })
}

pub(super) async fn login_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    jar: SignedCookieJar,
) -> Response {
    use openidconnect::AuthenticationFlow;
    use openidconnect::CsrfToken;
    use openidconnect::Nonce;
    use openidconnect::PkceCodeChallenge;
    use openidconnect::core::CoreResponseType;

    let Some(runtime) = &state.oidc else {
        return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
    };

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let return_to = match query.get("return_to") {
        Some(raw) => safe_return_to(raw),
        None => "/".to_string(),
    };

    let mut auth_request = runtime.client.authorize_url(
        AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
        CsrfToken::new_random,
        Nonce::new_random,
    );
    for scope in &runtime.scopes {
        auth_request = auth_request.add_scope(scope.clone());
    }
    let (authorize_url, csrf_token, nonce) = auth_request.set_pkce_challenge(pkce_challenge).url();

    let flow_state = OidcFlowState {
        csrf_token: csrf_token.secret().clone(),
        nonce: nonce.secret().clone(),
        pkce_verifier: pkce_verifier.secret().clone(),
        return_to,
    };
    let jar = set_flow_cookie(jar, &flow_state, runtime.secure_cookies);

    (jar, axum::response::Redirect::to(authorize_url.as_str())).into_response()
}

pub(super) async fn callback_handler(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
    jar: SignedCookieJar,
) -> Response {
    use openidconnect::AuthorizationCode;
    use openidconnect::Nonce;
    use openidconnect::PkceCodeVerifier;
    use openidconnect::TokenResponse;

    let Some(runtime) = &state.oidc else {
        return (StatusCode::NOT_FOUND, "OIDC is not configured").into_response();
    };

    let Some((jar, flow)) = take_flow_cookie(jar) else {
        return (
            StatusCode::BAD_REQUEST,
            "Login session expired or was not started here. Try logging in again.",
        )
            .into_response();
    };

    let Some(code) = query.get("code").cloned() else {
        return (StatusCode::BAD_REQUEST, "Missing authorization code").into_response();
    };
    let returned_state = query.get("state").cloned().unwrap_or_default();
    if returned_state != flow.csrf_token {
        return (
            StatusCode::BAD_REQUEST,
            "Login state mismatch — please try again",
        )
            .into_response();
    }

    let http_client = match openidconnect::reqwest::ClientBuilder::new()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("OIDC HTTP client error: {e}"),
            )
                .into_response();
        }
    };

    let exchange = match runtime.client.exchange_code(AuthorizationCode::new(code)) {
        Ok(request) => request,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("failed to build token exchange request: {e}"),
            )
                .into_response();
        }
    };
    let token_response = match exchange
        .set_pkce_verifier(PkceCodeVerifier::new(flow.pkce_verifier))
        .request_async(&http_client)
        .await
    {
        Ok(response) => response,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("OIDC token exchange failed: {e}"),
            )
                .into_response();
        }
    };

    let Some(id_token) = token_response.id_token() else {
        return (
            StatusCode::BAD_GATEWAY,
            "OIDC provider did not return an ID token",
        )
            .into_response();
    };
    let claims = match id_token.claims(&runtime.client.id_token_verifier(), &Nonce::new(flow.nonce))
    {
        Ok(claims) => claims,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("ID token validation failed: {e}"),
            )
                .into_response();
        }
    };

    let Some(email) = claims.email() else {
        return (
            StatusCode::BAD_GATEWAY,
            "OIDC provider did not return an email claim; add the `email` scope on the \
             provider side",
        )
            .into_response();
    };
    let email = email.as_str().to_lowercase();

    let jar = set_session_cookie(jar, &email, runtime.secure_cookies);
    // The flow cookie is signed, so `return_to` cannot have been tampered
    // with since `login_handler` validated it — re-checking here keeps the
    // guarantee local to the redirect that acts on it.
    (
        jar,
        axum::response::Redirect::to(&safe_return_to(&flow.return_to)),
    )
        .into_response()
}

pub(super) async fn logout_handler(
    jar: SignedCookieJar,
) -> (SignedCookieJar, axum::response::Redirect) {
    let jar = clear_session_cookie(jar);
    // RP-initiated logout (redirecting to the IdP's own end-session
    // endpoint) would need discovering with a metadata type that carries
    // `end_session_endpoint`, which CoreProviderMetadata does not — out of
    // scope for now; clearing the local session cookie is sufficient to
    // log the user out of hashcards itself.
    let target = "/".to_string();
    (jar, axum::response::Redirect::to(&target))
}

/// Characters that must be percent-encoded when embedding an arbitrary path
/// in a `return_to` query parameter, matching the encode set `flash.rs`
/// already uses for the same purpose.
const RETURN_TO_ENCODE_SET: &percent_encoding::AsciiSet = percent_encoding::NON_ALPHANUMERIC;

pub(super) async fn require_auth(
    axum::extract::State(state): axum::extract::State<AppState>,
    jar: SignedCookieJar,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let secure = state.oidc.as_ref().is_some_and(|o| o.secure_cookies());
    match read_session(&jar) {
        Some(_) => {
            let jar = if session_needs_renewal(&jar) {
                match read_session(&jar) {
                    Some(user) => set_session_cookie(jar, &user.email, secure),
                    None => jar,
                }
            } else {
                jar
            };
            let response = next.run(request).await;
            (jar, response).into_response()
        }
        None => {
            let return_to = request.uri().to_string();
            let encoded = percent_encoding::utf8_percent_encode(&return_to, RETURN_TO_ENCODE_SET);
            axum::response::Redirect::to(&format!("/auth/login?return_to={encoded}"))
                .into_response()
        }
    }
}

pub(crate) struct MissingSession;

impl IntoResponse for MissingSession {
    fn into_response(self) -> Response {
        (StatusCode::UNAUTHORIZED, "Not logged in").into_response()
    }
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = MissingSession;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = SignedCookieJar::from_headers(&parts.headers, state.session_key.clone());
        read_session(&jar).ok_or(MissingSession)
    }
}

/// Lets handlers take `Option<CurrentUser>` directly — `None` when there is
/// no session, `Some` when there is one. axum's `Option<T>` extractor is
/// driven by this trait rather than a blanket bridge from
/// `FromRequestParts`, so it needs its own (infallible) impl.
impl axum::extract::OptionalFromRequestParts<AppState> for CurrentUser {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Option<Self>, Self::Rejection> {
        let jar = SignedCookieJar::from_headers(&parts.headers, state.session_key.clone());
        Ok(read_session(&jar))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use axum_extra::extract::cookie::Key;

    use super::*;

    #[test]
    fn test_session_round_trip() {
        let key = Key::generate();
        let jar = SignedCookieJar::new(key);
        let jar = set_session_cookie(jar, "me@example.com", true);
        let user = read_session(&jar).expect("session should be readable immediately after set");
        assert_eq!(user.email, "me@example.com");
    }

    #[test]
    fn test_expired_session_is_rejected() {
        let key = Key::generate();
        let jar = SignedCookieJar::new(key);
        let expired = Timestamp::now().minus_minutes(60 * 24);
        let payload = SessionPayload {
            email: "me@example.com".to_string(),
            expires_at: expired.to_string(),
        };
        let value = serde_json::to_string(&payload).expect("serializable");
        let jar = jar.add(Cookie::new(SESSION_COOKIE, value));
        assert!(read_session(&jar).is_none());
    }

    #[test]
    fn test_flow_round_trip() {
        let key = Key::generate();
        let jar = SignedCookieJar::new(key);
        let state = OidcFlowState {
            csrf_token: "csrf".to_string(),
            nonce: "nonce".to_string(),
            pkce_verifier: "verifier".to_string(),
            return_to: "/collection/japanese".to_string(),
        };
        let jar = set_flow_cookie(jar, &state, true);
        let (_, taken) = take_flow_cookie(jar).expect("flow cookie should be readable");
        assert_eq!(taken.csrf_token, "csrf");
        assert_eq!(taken.return_to, "/collection/japanese");
    }

    #[tokio::test]
    async fn test_build_oidc_runtime_fails_clearly_on_bad_issuer() {
        let config = ResolvedOidc {
            issuer_url: "http://127.0.0.1:1".to_string(), // nothing listens here
            client_id: "abc".to_string(),
            client_secret: "secret".to_string(),
            external_url: "https://hashcards.example.com".to_string(),
            session_secret: "a-very-long-random-session-secret-value".to_string(),
            scopes: vec!["openid".to_string(), "email".to_string()],
        };
        let result = build_oidc_runtime(&config).await;
        assert!(
            result.is_err(),
            "expected discovery against a closed port to fail"
        );
    }

    // ── Mock OIDC provider ──────────────────────────────────────────────
    //
    // A minimal stand-in IdP for exercising the full login round trip
    // without a real Nextcloud. It signs ID tokens with HS256, using the
    // client secret as the HMAC key (an OIDC-standard "confidential
    // client" signing mode openidconnect itself supports) — this avoids
    // needing an RSA keypair and a JWKS endpoint with real key material,
    // while still exercising the whole discovery -> authorize -> token ->
    // ID-token-verification chain for real.

    #[derive(Clone)]
    struct MockIdpState {
        issuer: String,
        client_secret: String,
        /// The `nonce` from the most recent `/authorize` request, so `/token`
        /// can embed the same value in the ID token it issues (a real IdP
        /// would derive this from the authorization code it minted).
        last_nonce: std::sync::Arc<Mutex<Option<String>>>,
        email: String,
    }

    #[derive(serde::Serialize)]
    struct MockClaims {
        iss: String,
        sub: String,
        aud: String,
        exp: i64,
        iat: i64,
        nonce: Option<String>,
        email: String,
    }

    async fn mock_discovery_handler(
        axum::extract::State(state): axum::extract::State<MockIdpState>,
    ) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "issuer": state.issuer,
            "authorization_endpoint": format!("{}/authorize", state.issuer),
            "token_endpoint": format!("{}/token", state.issuer),
            "jwks_uri": format!("{}/jwks", state.issuer),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["HS256"],
            "scopes_supported": ["openid", "email", "profile"],
        }))
    }

    async fn mock_jwks_handler() -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({ "keys": [] }))
    }

    async fn mock_authorize_handler(
        axum::extract::State(state): axum::extract::State<MockIdpState>,
        axum::extract::Query(query): axum::extract::Query<
            std::collections::HashMap<String, String>,
        >,
    ) -> axum::response::Redirect {
        *state.last_nonce.lock().unwrap() = query.get("nonce").cloned();
        let redirect_uri = query.get("redirect_uri").cloned().unwrap_or_default();
        let oauth_state = query.get("state").cloned().unwrap_or_default();
        axum::response::Redirect::to(&format!(
            "{redirect_uri}?code=test-code&state={oauth_state}"
        ))
    }

    async fn mock_token_handler(
        axum::extract::State(state): axum::extract::State<MockIdpState>,
    ) -> axum::Json<serde_json::Value> {
        let nonce = state.last_nonce.lock().unwrap().clone();
        let now = chrono::Utc::now().timestamp();
        let claims = MockClaims {
            iss: state.issuer.clone(),
            sub: state.email.clone(),
            aud: "test-client".to_string(),
            exp: now + 300,
            iat: now,
            nonce,
            email: state.email.clone(),
        };
        let id_token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::HS256),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(state.client_secret.as_bytes()),
        )
        .expect("HS256 signing cannot fail for well-formed claims");
        axum::Json(serde_json::json!({
            "access_token": "test-access-token",
            "token_type": "Bearer",
            "expires_in": 300,
            "id_token": id_token,
        }))
    }

    /// Starts a mock OIDC provider on a local port, claiming `email` for
    /// whoever logs in. Returns the port.
    async fn spawn_mock_oidc_provider(email: &str, client_secret: &str) -> Fallible<u16> {
        let port = portpicker::pick_unused_port().expect("no free port for mock IdP");
        let state = MockIdpState {
            issuer: format!("http://127.0.0.1:{port}"),
            client_secret: client_secret.to_string(),
            last_nonce: std::sync::Arc::new(Mutex::new(None)),
            email: email.to_string(),
        };
        let app = axum::Router::new()
            .route(
                "/.well-known/openid-configuration",
                axum::routing::get(mock_discovery_handler),
            )
            .route("/jwks", axum::routing::get(mock_jwks_handler))
            .route("/authorize", axum::routing::get(mock_authorize_handler))
            .route("/token", axum::routing::post(mock_token_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        crate::utils::wait_for_server("127.0.0.1", port).await?;
        Ok(port)
    }

    #[tokio::test]
    async fn test_oidc_login_round_trip() -> Fallible<()> {
        use crate::cmd::serve::config::DefaultsSection;
        use crate::cmd::serve::config::ResolvedCollection;
        use crate::cmd::serve::config::ResolvedServeConfig;
        use crate::cmd::serve::server::start_serve;

        let client_secret = "test-client-secret-value";
        let idp_port = spawn_mock_oidc_provider("me@example.com", client_secret).await?;

        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;

        let port = portpicker::pick_unused_port().expect("no free port for serve");
        let config = ResolvedServeConfig {
            host: "127.0.0.1".to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Test Collection".to_string(),
                slug: "test-collection".to_string(),
                coll_dir: dir.path().to_path_buf(),
                db_path: dir.path().join("hashcards.db"),
                owner: Some("me@example.com".to_string()),
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: Some(ResolvedOidc {
                issuer_url: format!("http://127.0.0.1:{idp_port}"),
                client_id: "test-client".to_string(),
                client_secret: client_secret.to_string(),
                external_url: format!("http://127.0.0.1:{port}"),
                session_secret: "a-very-long-random-session-secret-value".to_string(),
                scopes: vec!["openid".to_string(), "email".to_string()],
            }),
            _temp_dir: None,
        };
        tokio::spawn(async move { start_serve(config).await });
        crate::utils::wait_for_server("127.0.0.1", port).await?;

        let client = reqwest::Client::builder().cookie_store(true).build()?;

        // Unauthenticated: /collection/test-collection -> /auth/login ->
        // mock /authorize -> /auth/callback -> back to the collection page,
        // all followed automatically as ordinary HTTP redirects.
        let response = client
            .get(format!(
                "http://127.0.0.1:{port}/collection/test-collection"
            ))
            .send()
            .await?;
        assert!(
            response.status().is_success(),
            "status: {}",
            response.status()
        );
        let body = response.text().await?;
        assert!(body.contains("Test Collection"), "body: {body}");
        Ok(())
    }

    #[tokio::test]
    async fn test_cross_user_collection_access_is_404() -> Fallible<()> {
        use crate::cmd::serve::config::DefaultsSection;
        use crate::cmd::serve::config::ResolvedCollection;
        use crate::cmd::serve::config::ResolvedServeConfig;
        use crate::cmd::serve::server::start_serve;

        let client_secret = "test-client-secret-value";
        // The mock always claims this email — every login in this test is
        // "as Alice."
        let idp_port = spawn_mock_oidc_provider("alice@example.com", client_secret).await?;

        let alice_dir = tempfile::tempdir()?;
        std::fs::write(alice_dir.path().join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let bob_dir = tempfile::tempdir()?;
        std::fs::write(bob_dir.path().join("Beta.md"), "Q: What is 2+2?\nA: 4\n")?;

        let port = portpicker::pick_unused_port().expect("no free port for serve");
        let config = ResolvedServeConfig {
            host: "127.0.0.1".to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![
                ResolvedCollection {
                    name: "Alice's Deck".to_string(),
                    slug: "alice-deck".to_string(),
                    coll_dir: alice_dir.path().to_path_buf(),
                    db_path: alice_dir.path().join("hashcards.db"),
                    owner: Some("alice@example.com".to_string()),
                },
                ResolvedCollection {
                    name: "Bob's Deck".to_string(),
                    slug: "bob-deck".to_string(),
                    coll_dir: bob_dir.path().to_path_buf(),
                    db_path: bob_dir.path().join("hashcards.db"),
                    owner: Some("bob@example.com".to_string()),
                },
            ],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: Some(ResolvedOidc {
                issuer_url: format!("http://127.0.0.1:{idp_port}"),
                client_id: "test-client".to_string(),
                client_secret: client_secret.to_string(),
                external_url: format!("http://127.0.0.1:{port}"),
                session_secret: "a-very-long-random-session-secret-value".to_string(),
                scopes: vec!["openid".to_string(), "email".to_string()],
            }),
            _temp_dir: None,
        };
        tokio::spawn(async move { start_serve(config).await });
        crate::utils::wait_for_server("127.0.0.1", port).await?;

        let client = reqwest::Client::builder().cookie_store(true).build()?;

        // Log in as Alice by visiting her own collection first.
        let response = client
            .get(format!("http://127.0.0.1:{port}/collection/alice-deck"))
            .send()
            .await?;
        assert!(response.status().is_success());

        // Alice requesting Bob's slug gets 404, not Bob's content.
        let response = client
            .get(format!("http://127.0.0.1:{port}/collection/bob-deck"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

        // The landing page lists only Alice's collection.
        let response = client
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await?;
        let body = response.text().await?;
        assert!(body.contains("Alice's Deck"), "body: {body}");
        assert!(!body.contains("Bob's Deck"), "body: {body}");
        Ok(())
    }

    #[tokio::test]
    async fn test_logout_clears_session() -> Fallible<()> {
        use crate::cmd::serve::config::DefaultsSection;
        use crate::cmd::serve::config::ResolvedCollection;
        use crate::cmd::serve::config::ResolvedServeConfig;
        use crate::cmd::serve::server::start_serve;

        let client_secret = "test-client-secret-value";
        let idp_port = spawn_mock_oidc_provider("me@example.com", client_secret).await?;

        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;

        let port = portpicker::pick_unused_port().expect("no free port for serve");
        let config = ResolvedServeConfig {
            host: "127.0.0.1".to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Test Collection".to_string(),
                slug: "test-collection".to_string(),
                coll_dir: dir.path().to_path_buf(),
                db_path: dir.path().join("hashcards.db"),
                owner: Some("me@example.com".to_string()),
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: Some(ResolvedOidc {
                issuer_url: format!("http://127.0.0.1:{idp_port}"),
                client_id: "test-client".to_string(),
                client_secret: client_secret.to_string(),
                external_url: format!("http://127.0.0.1:{port}"),
                session_secret: "a-very-long-random-session-secret-value".to_string(),
                scopes: vec!["openid".to_string(), "email".to_string()],
            }),
            _temp_dir: None,
        };
        tokio::spawn(async move { start_serve(config).await });
        crate::utils::wait_for_server("127.0.0.1", port).await?;

        // Redirects disabled so each hop's own response (and its Set-Cookie
        // headers) is inspectable; the cookie jar still accumulates cookies
        // across these manual requests since it lives on the client, not
        // tied to redirect-following.
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        // Manually follow the login chain: collection -> /auth/login ->
        // mock /authorize -> /auth/callback -> collection.
        let mut url = format!("http://127.0.0.1:{port}/collection/test-collection");
        for _ in 0..5 {
            let response = client.get(&url).send().await?;
            if response.status().is_success() {
                break;
            }
            url = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(|s| {
                    if s.starts_with('/') {
                        format!("http://127.0.0.1:{port}{s}")
                    } else {
                        s.to_string()
                    }
                })
                .ok_or_else(|| ErrorReport::new("expected a Location header while logging in"))?;
        }

        // Now logged in: /auth/logout's own response (not a followed
        // redirect target) must clear the session cookie.
        let response = client
            .post(format!("http://127.0.0.1:{port}/auth/logout"))
            .send()
            .await?;
        let cookies: Vec<String> = response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap_or_default().to_string())
            .collect();
        let jar_has_session = cookies.iter().any(|s| {
            s.starts_with("hc_session=") && (s.contains("Max-Age=0") || s.contains("hc_session=;"))
        });
        assert!(
            jar_has_session,
            "expected /auth/logout to clear the hc_session cookie, got: {cookies:?}"
        );
        Ok(())
    }

    /// `return_to` must never carry the user off this server after a
    /// successful login: an absolute or protocol-relative target is an open
    /// redirect, and falls back to the site root.
    #[test]
    fn test_return_to_rejects_offsite_targets() {
        assert_eq!(
            safe_return_to("/collection/japanese"),
            "/collection/japanese"
        );
        assert_eq!(safe_return_to("/a?b=c#d"), "/a?b=c#d");
        assert_eq!(safe_return_to("https://evil.example/"), "/");
        assert_eq!(safe_return_to("//evil.example/"), "/");
        assert_eq!(safe_return_to("/\\evil.example/"), "/");
        assert_eq!(safe_return_to("evil.example"), "/");
        assert_eq!(safe_return_to(""), "/");
        // Control characters cannot go in a `Location` header.
        assert_eq!(safe_return_to("/ok\r\nSet-Cookie: x=1"), "/");
    }

    /// The session cookie must outlive the browser process: without an
    /// explicit lifetime it is a session cookie and the user is logged out on
    /// every restart, contradicting the 30-day sliding session.
    #[test]
    fn test_session_cookie_carries_its_lifetime_and_secure_flag() {
        let key = Key::generate();
        let jar = SignedCookieJar::new(key);
        let jar = set_session_cookie(jar, "me@example.com", true);
        let cookie = jar.get(SESSION_COOKIE).expect("cookie was just set");
        assert_eq!(
            cookie.max_age(),
            Some(CookieDuration::minutes(SESSION_LIFETIME_MINUTES))
        );
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.http_only(), Some(true));
    }

    /// A plain-HTTP deployment must not set `Secure`, or the cookie is never
    /// sent back and login loops forever.
    #[test]
    fn test_session_cookie_is_not_secure_over_plain_http() {
        let key = Key::generate();
        let jar = SignedCookieJar::new(key);
        let jar = set_session_cookie(jar, "me@example.com", false);
        let cookie = jar.get(SESSION_COOKIE).expect("cookie was just set");
        assert_ne!(cookie.secure(), Some(true));
    }
}
