use percent_encoding::AsciiSet;
use percent_encoding::CONTROLS;
use percent_encoding::utf8_percent_encode;

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

/// Percent-encode a relative file path for use in a URL path.
///
/// `/` is left alone — it separates the path's own components — but
/// everything that would end the path or change its meaning is encoded.
/// `validate_name` allows `#` and `?` in a file name, and left raw the
/// browser would treat them as a fragment or query and ask for the wrong
/// file entirely.
pub fn encoded_path(rel: &str) -> String {
    utf8_percent_encode(rel, PATH_RESERVED).to_string()
}

/// Everything unsafe in a URL path except `/`.
const PATH_RESERVED: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'+')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_what_would_end_a_url_path() {
        assert_eq!(encoded_path("a/b.md"), "a/b.md");
        assert_eq!(encoded_path("a#b?c.md"), "a%23b%3Fc.md");
        assert_eq!(
            encoded_path("my cards/día 1.md"),
            "my%20cards/d%C3%ADa%201.md"
        );
    }

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
