/// Return the URL if it is safe to render into an `href` attribute
/// (http or https scheme only), otherwise `None`.
///
/// Defense in depth for BUG-24: URLs are validated when stored, but a
/// hand-edited config file, pre-existing persisted data, or a future
/// refactor must not be able to smuggle a `javascript:` (or other unsafe
/// scheme) URL into a link.
pub fn safe_href(url: &str) -> Option<&str> {
    let trimmed = url.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        Some(trimmed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https() {
        assert_eq!(safe_href("https://x.com/a"), Some("https://x.com/a"));
        assert_eq!(safe_href("http://x.com/a"), Some("http://x.com/a"));
        assert_eq!(safe_href("  https://x.com  "), Some("https://x.com"));
    }

    #[test]
    fn rejects_everything_else() {
        assert_eq!(safe_href("javascript:alert(1)"), None);
        assert_eq!(safe_href("JavaScript:alert(1)"), None);
        assert_eq!(safe_href(" javascript:alert(1)"), None);
        assert_eq!(safe_href("data:text/html,x"), None);
        assert_eq!(safe_href("/relative/path"), None);
        assert_eq!(safe_href(""), None);
    }
}
