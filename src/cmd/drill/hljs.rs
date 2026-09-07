//! highlight.js, served from the binary under this build's revision.
//!
//! Script and theme are one vendored package and share a revision, so a bump
//! moves both URLs at once and no client can pair the new script with the
//! old theme.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::http::header::CONTENT_TYPE;

use crate::utils::CACHE_CONTROL_REVALIDATE;
use crate::utils::revision;
use crate::utils::revisioned_cache_control;

const HLJS_JS: &[u8] = include_bytes!("../../../vendor/highlightjs/highlight.min.js");
const HLJS_CSS: &[u8] = include_bytes!("../../../vendor/highlightjs/github.min.css");

pub static HLJS_REV: LazyLock<String> = LazyLock::new(|| revision(&[HLJS_JS, HLJS_CSS]));

pub static HLJS_JS_URL: LazyLock<String> =
    LazyLock::new(|| format!("/hljs/{}/highlight.js", *HLJS_REV));
pub static HLJS_CSS_URL: LazyLock<String> =
    LazyLock::new(|| format!("/hljs/{}/github.css", *HLJS_REV));

type Response = (StatusCode, [(HeaderName, &'static str); 2], &'static [u8]);

fn serve(
    bytes: &'static [u8],
    content_type: &'static str,
    cache_control: &'static str,
) -> Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, content_type), (CACHE_CONTROL, cache_control)],
        bytes,
    )
}

pub async fn hljs_js_handler(Path(rev): Path<String>) -> Response {
    serve(
        HLJS_JS,
        "text/javascript",
        revisioned_cache_control(&rev, &HLJS_REV),
    )
}

pub async fn hljs_css_handler(Path(rev): Path<String>) -> Response {
    serve(
        HLJS_CSS,
        "text/css",
        revisioned_cache_control(&rev, &HLJS_REV),
    )
}

/// The old unrevisioned paths, for HTML rendered before highlight.js was
/// served under a revision. They revalidate: the path does not name the
/// bytes, so nothing may be kept against it.
pub async fn legacy_hljs_js_handler() -> Response {
    serve(HLJS_JS, "text/javascript", CACHE_CONTROL_REVALIDATE)
}

pub async fn legacy_hljs_css_handler() -> Response {
    serve(HLJS_CSS, "text/css", CACHE_CONTROL_REVALIDATE)
}

#[cfg(test)]
mod tests {
    use super::HLJS_CSS_URL;
    use super::HLJS_JS_URL;
    use super::HLJS_REV;

    #[test]
    fn the_asset_urls_carry_the_revision() {
        assert_eq!(*HLJS_JS_URL, format!("/hljs/{}/highlight.js", *HLJS_REV));
        assert_eq!(*HLJS_CSS_URL, format!("/hljs/{}/github.css", *HLJS_REV));
    }
}
