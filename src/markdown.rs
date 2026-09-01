// Copyright 2025 Fernando Borretti
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use percent_encoding::AsciiSet;
use percent_encoding::CONTROLS;
use percent_encoding::utf8_percent_encode;
use pulldown_cmark::CowStr;
use pulldown_cmark::Event;
use pulldown_cmark::Options;
use pulldown_cmark::Parser;
use pulldown_cmark::Tag;
use pulldown_cmark::html::push_html;
use reqwest::Url;

use crate::error::ErrorReport;
use crate::error::Fallible;
use crate::media::resolve::MediaResolver;

/// Characters that must be percent-encoded in a URL path segment.
/// Encodes control characters plus space, #, ?, %, and / (RFC 3986).
const PATH_SEGMENT: &AsciiSet = &CONTROLS.add(b' ').add(b'#').add(b'?').add(b'%').add(b'/');

const AUDIO_EXTENSIONS: [&str; 4] = ["mp3", "wav", "ogg", "m4a"];

fn is_audio_file(url: &str) -> bool {
    if let Some(ext) = url.split('.').next_back() {
        AUDIO_EXTENSIONS.contains(&ext)
    } else {
        false
    }
}

/// Escape a string for interpolation into a double-quoted HTML attribute.
fn escape_attribute(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Configuration for Markdown rendering.
pub struct MarkdownRenderConfig {
    /// A media resolver.
    pub resolver: MediaResolver,
    /// URL prefix for file serving (e.g. "http://localhost:8000/file" or "/collection/slug/file").
    pub file_url_prefix: String,
}

pub fn markdown_to_html(config: &MarkdownRenderConfig, markdown: &str) -> Fallible<String> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_MATH);
    let parser = Parser::new_ext(markdown, options);
    let events: Vec<Event<'_>> = parser
        .map(|event| match event {
            Event::Start(Tag::Image {
                link_type,
                title,
                dest_url,
                id,
            }) => {
                let url = modify_url(&dest_url, config)?;
                // Does the URL point to an audio file?
                let ev = if is_audio_file(&url) {
                    // If so, render it as an HTML5 audio element.
                    Event::Html(CowStr::Boxed(
                        format!(
                            r#"<audio controls src="{}" title="{}"></audio>"#,
                            // BUG-23: `PATH_SEGMENT` does not encode `"`, so
                            // the URL needs attribute escaping just as much as
                            // the title does. Without it a media file whose
                            // name contains a quote closes `src` early and
                            // injects arbitrary attributes.
                            escape_attribute(&url),
                            escape_attribute(&title)
                        )
                        .into_boxed_str(),
                    ))
                } else {
                    // Treat it as a normal image.
                    Event::Start(Tag::Image {
                        link_type,
                        title,
                        dest_url: CowStr::Boxed(url.into_boxed_str()),
                        id,
                    })
                };
                Ok(ev)
            }
            _ => Ok(event),
        })
        .collect::<Fallible<Vec<_>>>()?;
    let mut html_output: String = String::new();
    push_html(&mut html_output, events.into_iter());
    Ok(html_output)
}

pub fn markdown_to_html_inline(config: &MarkdownRenderConfig, markdown: &str) -> Fallible<String> {
    let text = markdown_to_html(config, markdown)?;
    if text.starts_with("<p>") && text.ends_with("</p>\n") {
        let len = text.len();
        Ok(text[3..len - 5].to_string())
    } else {
        Ok(text)
    }
}

fn modify_url(url: &str, config: &MarkdownRenderConfig) -> Fallible<String> {
    use crate::media::resolve::ResolveError;
    let prefix = config.file_url_prefix.trim_end_matches('/');
    let resolved = match config.resolver.resolve(url) {
        Ok(p) => p,
        // External URLs (e.g. HedgeDoc image uploads) are passed through after
        // parsing and re-serializing. This rejects invalid URLs (including those
        // containing whitespace or control characters) and non-http(s) schemes,
        // and produces a canonicalized string that is safe to embed in HTML.
        Err(ResolveError::ExternalUrl) => {
            let parsed = Url::parse(url).map_err(|err| {
                ErrorReport::new(format!(
                    "External media URL is invalid ('{}'): {}",
                    url, err
                ))
            })?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                return Err(ErrorReport::new(format!(
                    "External media URL must use http or https (got: {})",
                    url
                )));
            }
            return Ok(parsed.to_string());
        }
        Err(err) => {
            return Err(ErrorReport::new(format!(
                "Failed to resolve media path '{}': {}",
                url, err
            )));
        }
    };
    // Build a percent-encoded, forward-slash-separated URL path.
    let path: String = resolved
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .map(|seg| utf8_percent_encode(&seg, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("{prefix}/{path}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::helper::create_tmp_directory;
    use crate::media::resolve::MediaResolverBuilder;

    fn make_test_config() -> Fallible<MarkdownRenderConfig> {
        let coll_path: PathBuf = create_tmp_directory()?;
        let abs_deck_path: PathBuf = coll_path.join("deck.md");
        let image_path: PathBuf = coll_path.join("image.png");
        let audio_path: PathBuf = coll_path.join("audio.mp3");
        std::fs::write(&abs_deck_path, "")?;
        std::fs::write(&image_path, "")?;
        std::fs::write(&audio_path, "")?;
        let config = MarkdownRenderConfig {
            resolver: MediaResolverBuilder::new()
                .with_collection_path(coll_path)?
                .with_deck_path(PathBuf::from("deck.md"))?
                .build()?,
            file_url_prefix: "http://localhost:1234/file".to_string(),
        };
        Ok(config)
    }

    #[test]
    fn test_markdown_to_html() -> Fallible<()> {
        let markdown = "![alt](@/image.png)";
        let config = make_test_config()?;
        let html = markdown_to_html(&config, markdown)?;
        assert_eq!(
            html,
            "<p><img src=\"http://localhost:1234/file/image.png\" alt=\"alt\" /></p>\n"
        );
        Ok(())
    }

    /// Regression: a media filename containing a double quote must not be
    /// able to close the `src` attribute and inject further attributes into
    /// the generated `<audio>` element.
    ///
    /// Windows-only skip: `"` is one of the characters Windows filesystems
    /// refuse in a filename outright, so the attack this guards against
    /// cannot be constructed there in the first place. The same escaping
    /// path is still exercised, filename-independent, by
    /// `test_audio_title_is_attribute_escaped`.
    #[cfg(not(windows))]
    #[test]
    fn test_audio_url_is_attribute_escaped() -> Fallible<()> {
        let coll_path: PathBuf = create_tmp_directory()?;
        let abs_deck_path: PathBuf = coll_path.join("deck.md");
        let evil_name = r#"x" onerror="alert(1).mp3"#;
        std::fs::write(&abs_deck_path, "")?;
        std::fs::write(coll_path.join(evil_name), "")?;
        let config = MarkdownRenderConfig {
            resolver: MediaResolverBuilder::new()
                .with_collection_path(coll_path)?
                .with_deck_path(PathBuf::from("deck.md"))?
                .build()?,
            file_url_prefix: "http://localhost:1234/file".to_string(),
        };
        // Angle-bracket destinations let a name with quotes and spaces
        // through CommonMark's inline-image syntax.
        let html = markdown_to_html(&config, &format!("![](<@/{evil_name}>)"))?;
        assert!(
            !html.contains(r#"onerror="alert(1)"#),
            "attribute injection through the audio src: {html}"
        );
        Ok(())
    }

    #[test]
    fn test_markdown_to_html_inline() -> Fallible<()> {
        let markdown = "This is **bold** text.";
        let config = make_test_config()?;
        let html = markdown_to_html_inline(&config, markdown)?;
        assert_eq!(html, "This is <strong>bold</strong> text.");
        Ok(())
    }

    #[test]
    fn test_markdown_to_html_inline_heading() -> Fallible<()> {
        let markdown = "# Foo";
        let config = make_test_config()?;
        let html = markdown_to_html_inline(&config, markdown)?;
        assert_eq!(html, "<h1>Foo</h1>\n");
        Ok(())
    }

    #[test]
    fn test_external_url_image_renders_unchanged() -> Fallible<()> {
        // External image URLs (e.g. HedgeDoc uploads) must be passed through
        // as-is so the browser fetches them directly.
        let markdown = "![alt](https://example.com/image.png)";
        let config = make_test_config()?;
        let html = markdown_to_html(&config, markdown)?;
        assert!(
            html.contains("https://example.com/image.png"),
            "external URL should appear unchanged in output: {html}"
        );
        Ok(())
    }

    #[test]
    fn test_non_http_external_url_is_rejected() -> Fallible<()> {
        // Only http/https external URLs are permitted to prevent unsafe schemes
        // (e.g. javascript:) from being embedded in HTML attributes.
        let markdown = "![alt](javascript://evil)";
        let config = make_test_config()?;
        let result = markdown_to_html(&config, markdown);
        assert!(result.is_err(), "non-http external URL should be rejected");
        Ok(())
    }

    #[test]
    fn test_audio_title_is_attribute_escaped() -> Fallible<()> {
        // Regression test for BUG-23: the markdown image title is interpolated
        // into an HTML attribute; `"` and `<` must be escaped.
        let markdown = r#"![alt](@/audio.mp3 "a \" <b> title")"#;
        let config = make_test_config()?;
        let html = markdown_to_html(&config, markdown)?;
        assert!(
            html.contains(r#"title="a &quot; &lt;b&gt; title""#),
            "title must be attribute-escaped: {html}"
        );
        assert!(
            !html.contains(r#"<b> title"#),
            "raw markup from the title must not appear in output: {html}"
        );
        Ok(())
    }
}
