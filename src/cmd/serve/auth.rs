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
pub(super) struct CurrentUser {
    pub email: String,
}

#[derive(Serialize, Deserialize)]
struct SessionPayload {
    email: String,
    /// `Timestamp` only implements `Serialize` (see `types::timestamp`), so
    /// the expiry is round-tripped through its string form.
    expires_at: String,
}

pub(super) fn set_session_cookie(jar: SignedCookieJar, email: &str) -> SignedCookieJar {
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
    jar.remove(Cookie::from(SESSION_COOKIE))
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

pub(super) fn set_flow_cookie(jar: SignedCookieJar, state: &OidcFlowState) -> SignedCookieJar {
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

pub(super) struct OidcRuntime {
    client: DiscoveredCoreClient,
    scopes: Vec<openidconnect::Scope>,
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

    Ok(OidcRuntime { client, scopes })
}

pub(super) struct MissingSession;

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

#[cfg(test)]
mod tests {
    use axum_extra::extract::cookie::Key;

    use super::*;

    #[test]
    fn test_session_round_trip() {
        let key = Key::generate();
        let jar = SignedCookieJar::new(key);
        let jar = set_session_cookie(jar, "me@example.com");
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
        let jar = set_flow_cookie(jar, &state);
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
}
