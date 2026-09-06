use percent_encoding::AsciiSet;
use percent_encoding::CONTROLS;
use percent_encoding::utf8_percent_encode;

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
}
