//! One-shot flash messages, shared by the drill and serve servers.
//!
//! A `Flash` travels across a redirect as query params
//! (`?flash=<percent-encoded>&kind=<success|error>`), is extracted from the
//! decoded query map on the next GET, and is rendered once as a dismissible
//! banner. It is never persisted: reloading the redirected-to URL re-shows
//! it, navigating away drops it.

use std::collections::HashMap;

use axum::response::Redirect;
use maud::Markup;
use maud::html;
use percent_encoding::NON_ALPHANUMERIC;
use percent_encoding::utf8_percent_encode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashKind {
    Success,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Flash {
    pub kind: FlashKind,
    pub message: String,
}

impl Flash {
    pub fn success(message: impl Into<String>) -> Self {
        Flash {
            kind: FlashKind::Success,
            message: message.into(),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Flash {
            kind: FlashKind::Error,
            message: message.into(),
        }
    }

    fn kind_str(&self) -> &'static str {
        match self.kind {
            FlashKind::Success => "success",
            FlashKind::Error => "error",
        }
    }

    /// The redirect target URL with the flash appended as query params.
    /// Separate from `redirect` so it can be unit-tested (axum's `Redirect`
    /// does not expose its target).
    fn redirect_url(&self, to: &str) -> String {
        let sep = if to.contains('?') { '&' } else { '?' };
        let encoded = utf8_percent_encode(&self.message, NON_ALPHANUMERIC);
        format!("{to}{sep}flash={encoded}&kind={}", self.kind_str())
    }

    /// Redirect carrying the flash as query params
    /// `?flash=<percent-encoded>&kind=<success|error>`.
    pub fn redirect(self, to: &str) -> Redirect {
        Redirect::to(&self.redirect_url(to))
    }

    /// Extract from request query params (one-shot: rendered once, not
    /// persisted). The map comes from axum's `Query` extractor, which has
    /// already percent-decoded the values.
    pub fn from_query(query: &HashMap<String, String>) -> Option<Flash> {
        let message = query.get("flash")?.clone();
        let kind = match query.get("kind").map(|s| s.as_str()) {
            Some("success") => FlashKind::Success,
            _ => FlashKind::Error,
        };
        Some(Flash { kind, message })
    }

    /// Dismissible banner: `div.flash.flash-success` / `div.flash.flash-error`.
    pub fn render(&self) -> Markup {
        let class = match self.kind {
            FlashKind::Success => "flash flash-success",
            FlashKind::Error => "flash flash-error",
        };
        html! {
            div class=(class) role="alert" {
                span.flash-message { (self.message) }
                button.flash-dismiss
                    type="button"
                    onclick="this.closest('.flash').remove()"
                    title="Dismiss"
                { "\u{00D7}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn test_redirect_url_percent_encodes_message() {
        let flash = Flash::error("100% bad & wrong?");
        let url = flash.redirect_url("/collection/foo");
        assert_eq!(
            url,
            "/collection/foo?flash=100%25%20bad%20%26%20wrong%3F&kind=error"
        );
    }

    #[test]
    fn test_redirect_url_appends_with_ampersand_when_query_exists() {
        let flash = Flash::success("ok");
        assert_eq!(flash.redirect_url("/x?a=1"), "/x?a=1&flash=ok&kind=success");
    }

    #[test]
    fn test_from_query_roundtrip() {
        // axum's Query extractor percent-decodes values before they reach us,
        // so from_query sees plain text.
        let mut query = HashMap::new();
        query.insert("flash".to_string(), "Bookmark removed.".to_string());
        query.insert("kind".to_string(), "success".to_string());
        let flash = Flash::from_query(&query).unwrap();
        assert_eq!(flash.kind, FlashKind::Success);
        assert_eq!(flash.message, "Bookmark removed.");
    }

    #[test]
    fn test_from_query_missing_flash_is_none() {
        let query = HashMap::new();
        assert!(Flash::from_query(&query).is_none());
    }

    #[test]
    fn test_from_query_unknown_kind_defaults_to_error() {
        let mut query = HashMap::new();
        query.insert("flash".to_string(), "boom".to_string());
        let flash = Flash::from_query(&query).unwrap();
        assert_eq!(flash.kind, FlashKind::Error);
    }

    #[test]
    fn test_render_success_banner() {
        let html = Flash::success("Saved.").render().into_string();
        assert!(html.contains("flash-success"));
        assert!(html.contains("Saved."));
    }

    #[test]
    fn test_render_error_banner() {
        let html = Flash::error("Nope.").render().into_string();
        assert!(html.contains("flash-error"));
        assert!(html.contains("Nope."));
    }

    #[test]
    fn test_render_escapes_html_in_message() {
        let html = Flash::error("<script>alert(1)</script>")
            .render()
            .into_string();
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
