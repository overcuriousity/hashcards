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

use std::sync::LazyLock;

use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CONTENT_TYPE;
use maud::DOCTYPE;
use maud::Markup;
use maud::html;

use crate::cmd::drill::fonts::FONT_DIR_URL;
use crate::cmd::drill::hljs::HLJS_CSS_URL;
use crate::cmd::drill::hljs::HLJS_JS_URL;
use crate::cmd::drill::katex::KATEX_CSS_URL;
use crate::cmd::drill::katex::KATEX_JS_URL;
use crate::cmd::drill::katex::KATEX_MHCHEM_JS_URL;
use crate::utils::revision;

const MANIFEST_JSON: &str = r##"{
  "name": "hashcards-web",
  "short_name": "hashcards",
  "display": "standalone",
  "start_url": "/",
  "theme_color": "#f2f0ea",
  "background_color": "#f8f6f1",
  "icons": [
    { "src": "/icons/icon-192.png", "sizes": "192x192", "type": "image/png" },
    { "src": "/icons/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any maskable" }
  ]
}"##;

/// The fixed font directory `style.css` is written against.
const FONT_DIR: &str = "url(\"/fonts/";

const STYLE_CSS_SOURCE: &[u8] = include_bytes!("style.css");

/// The stylesheet, with its font references pointed at the revisioned font
/// directory. Written by hand against the plain `/fonts/` path, because the
/// revision is a hash and cannot be typed into a source file.
pub static STYLE_CSS: LazyLock<String> = LazyLock::new(|| {
    String::from_utf8_lossy(STYLE_CSS_SOURCE)
        .replace(FONT_DIR, &format!("url(\"{}/", *FONT_DIR_URL))
});

/// The path the stylesheet is served from.
///
/// It names sixteen hex characters of the hash of the very bytes served, so
/// a build that changes the stylesheet — or the fonts it names — changes the
/// URL that asks for it. It has to: the response is `immutable`, which does
/// not merely permit a cache to skip revalidation but forbids it, so at a
/// fixed path a client that had fetched the stylesheet once ran it against
/// freshly rendered HTML for the next week. Two devices on the same server
/// would then disagree about the layout, and nothing shipped could be seen
/// on the device that had cached.
pub static STYLE_REV: LazyLock<String> = LazyLock::new(|| revision(&[STYLE_CSS.as_bytes()]));

pub static STYLE_URL: LazyLock<String> =
    LazyLock::new(|| format!("/style/{}/style.css", *STYLE_REV));

const ICON_192: &[u8] = include_bytes!("icon-192.png");
const ICON_512: &[u8] = include_bytes!("icon-512.png");

pub async fn manifest_handler() -> (StatusCode, [(HeaderName, &'static str); 1], &'static str) {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, "application/manifest+json")],
        MANIFEST_JSON,
    )
}

pub async fn icon_192_handler() -> (StatusCode, [(HeaderName, &'static str); 1], &'static [u8]) {
    (StatusCode::OK, [(CONTENT_TYPE, "image/png")], ICON_192)
}

pub async fn icon_512_handler() -> (StatusCode, [(HeaderName, &'static str); 1], &'static [u8]) {
    (StatusCode::OK, [(CONTENT_TYPE, "image/png")], ICON_512)
}

/// Applied to `<html>` before the first paint.
///
/// A stylesheet cannot know a stored choice and a deferred script runs after
/// the first paint, so either way the wrong theme flashes on every load — on
/// a phone, brightly. Small enough to cost nothing, and wrapped in a `try`
/// because a browser with storage disabled must still render the page.
pub const THEME_BOOT: &str = "try{var t=localStorage.getItem('hashcards.theme');\
if(t)document.documentElement.setAttribute('data-theme',t)}catch(e){}";

/// The one control that is on every page.
///
/// Rendered hidden and shown by `script.js`: without script it could not
/// remember a choice, and a switch that forgets is worse than none. The label
/// is filled in there too, since it names the destination rather than the
/// state.
pub fn theme_toggle() -> Markup {
    html! {
        button.theme-toggle type="button" data-theme-toggle="" hidden
            aria-label="Switch between the light and dark theme" {
            span data-theme-label="" { "Theme" }
        }
    }
}

pub fn page_template(body: Markup) -> Markup {
    page_template_with_script("/script.js", body)
}

pub fn page_template_with_script(script_url: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                script { (maud::PreEscaped(THEME_BOOT)) }
                // The browser paints its chrome from this before the
                // stylesheet arrives, so the two theme surfaces are named
                // here as well as in the tokens.
                meta name="theme-color" media="(prefers-color-scheme: light)" content="#f2f0ea";
                meta name="theme-color" media="(prefers-color-scheme: dark)" content="#14171d";
                title { "hashcards-web" }
                link rel="manifest" href="/manifest.json";
                link rel="stylesheet" href=(KATEX_CSS_URL.as_str());
                link rel="stylesheet" href=(HLJS_CSS_URL.as_str());
                script defer src=(KATEX_JS_URL.as_str()) {};
                script defer src=(KATEX_MHCHEM_JS_URL.as_str()) {};
                script defer src=(HLJS_JS_URL.as_str()) {};
                link rel="stylesheet" href=(STYLE_URL.as_str());
                style { ".card-content { opacity: 0; }" }
                noscript { style { ".card-content { opacity: 1; }" }}
            }
            body {
                (theme_toggle())
                (body)
                script src=(script_url) {};
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FONT_DIR_URL;
    use super::STYLE_CSS;
    use super::STYLE_URL;
    use super::page_template;

    /// The stylesheet is served `immutable`, which forbids a cache from
    /// revalidating it. That is only safe while the path names the bytes.
    #[test]
    fn test_stylesheet_url_names_its_contents() {
        let hash = blake3::hash(STYLE_CSS.as_bytes()).to_hex();
        let expected = format!("/style/{}/style.css", &hash[..16]);
        assert_eq!(STYLE_URL.as_str(), expected);
    }

    /// The stylesheet is `immutable`, so a font it names has to move when the
    /// font changes — and the stylesheet's own hash then moves with it.
    #[test]
    fn test_stylesheet_asks_for_revisioned_fonts() {
        assert!(
            STYLE_CSS.contains(&format!("url(\"{}/inter-400.woff2\")", *FONT_DIR_URL)),
            "the stylesheet still names a font at the unrevisioned path"
        );
        assert!(
            !STYLE_CSS.contains("url(\"/fonts/inter"),
            "a font reference was left at the unrevisioned path"
        );
    }

    /// A page that still asks for the fixed path would be served a stylesheet
    /// its client is entitled to keep for a week.
    #[test]
    fn test_page_links_the_hashed_stylesheet() {
        let html = page_template(maud::html! { div {} }).into_string();
        assert!(
            html.contains(&format!(r#"href="{}""#, STYLE_URL.as_str())),
            "the hashed stylesheet is not linked: {html}"
        );
        assert!(
            !html.contains(r#"href="/style.css""#),
            "the fixed stylesheet path is still linked: {html}"
        );
    }

    /// `.end-link` is a bare class and the grade-button rules are not, so a
    /// selector of theirs that matches the End button wins every declaration
    /// the two share and the way out of a session renders as a fifth grade.
    /// They must not match it at all.
    #[test]
    fn test_grade_button_rules_do_not_match_the_end_link() {
        let css: &str = &STYLE_CSS;
        assert!(css.contains(".end-link {"), "the End button lost its rule");
        for block in css.split('}') {
            let Some((selectors, _)) = block.split_once('{') else {
                continue;
            };
            for selector in selectors.split(',') {
                let selector = selector.trim();
                // Comments carry example selectors; only real ones matter.
                if selector.contains("/*") || !selector.contains(".controls button") {
                    continue;
                }
                assert!(
                    selector.contains(":not(.end-link)"),
                    "`{selector}` also matches the End button"
                );
            }
        }
    }
}
