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

//! KaTeX, served from the binary at paths that name this build's copy of it.
//!
//! The stylesheet, the scripts and the fonts are one vendored package and
//! are revisioned as one: bumping KaTeX changes every URL here at once, so a
//! client can never end up running last week's script against this week's
//! stylesheet. The stylesheet's own `url(/katex/fonts/...)` references are
//! rewritten at startup to carry the revision, which is why `KATEX_CSS` is a
//! `String` rather than the bytes on disk.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::http::header::CONTENT_TYPE;

use crate::utils::CACHE_CONTROL_REVALIDATE;
use crate::utils::revision;
use crate::utils::revisioned_cache_control;

const KATEX_CSS_SOURCE: &[u8] = include_bytes!("../../../vendor/katex/katex.min.css");
const KATEX_JS: &[u8] = include_bytes!("../../../vendor/katex/katex.min.js");
const KATEX_MHCHEM_JS: &[u8] = include_bytes!("../../../vendor/katex/contrib/mhchem.min.js");

/// The fixed font directory the vendored stylesheet was built against.
const KATEX_FONT_DIR: &str = "url(/katex/fonts/";

pub static KATEX_REV: LazyLock<String> =
    LazyLock::new(|| revision(&[KATEX_CSS_SOURCE, KATEX_JS, KATEX_MHCHEM_JS]));

/// The vendored stylesheet with its font references pointed at the
/// revisioned directory. Vendored CSS is minified and not ours to edit, so
/// the one thing that has to change about it is changed here instead.
pub static KATEX_CSS: LazyLock<String> = LazyLock::new(|| {
    let css = String::from_utf8_lossy(KATEX_CSS_SOURCE);
    css.replace(KATEX_FONT_DIR, &format!("url(/katex/fonts/{}/", *KATEX_REV))
});

pub static KATEX_JS_URL: LazyLock<String> =
    LazyLock::new(|| format!("/katex/{}/katex.js", *KATEX_REV));
pub static KATEX_MHCHEM_JS_URL: LazyLock<String> =
    LazyLock::new(|| format!("/katex/{}/mhchem.js", *KATEX_REV));
pub static KATEX_CSS_URL: LazyLock<String> =
    LazyLock::new(|| format!("/katex/{}/katex.css", *KATEX_REV));

type Response = (StatusCode, [(HeaderName, &'static str); 2], &'static [u8]);

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [
            (CONTENT_TYPE, "text/plain"),
            (CACHE_CONTROL, CACHE_CONTROL_REVALIDATE),
        ],
        b"Not Found",
    )
}

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

/// The rewritten stylesheet, as bytes that live as long as the process.
fn katex_css_bytes() -> &'static [u8] {
    KATEX_CSS.as_bytes()
}

pub async fn katex_css_handler(Path(rev): Path<String>) -> Response {
    serve(
        katex_css_bytes(),
        "text/css",
        revisioned_cache_control(&rev, &KATEX_REV),
    )
}

pub async fn katex_js_handler(Path(rev): Path<String>) -> Response {
    serve(
        KATEX_JS,
        "text/javascript",
        revisioned_cache_control(&rev, &KATEX_REV),
    )
}

pub async fn katex_mhchem_js_handler(Path(rev): Path<String>) -> Response {
    serve(
        KATEX_MHCHEM_JS,
        "text/javascript",
        revisioned_cache_control(&rev, &KATEX_REV),
    )
}

pub async fn katex_font_handler(Path((rev, name)): Path<(String, String)>) -> Response {
    match katex_font_bytes(&name) {
        Some(bytes) => serve(
            bytes,
            "font/woff2",
            revisioned_cache_control(&rev, &KATEX_REV),
        ),
        None => not_found(),
    }
}

/// The old unrevisioned paths, for HTML rendered before KaTeX was served
/// under a revision. Nothing may be cached against a name that does not
/// describe its contents, so these revalidate.
pub async fn legacy_katex_css_handler() -> Response {
    serve(katex_css_bytes(), "text/css", CACHE_CONTROL_REVALIDATE)
}

pub async fn legacy_katex_js_handler() -> Response {
    serve(KATEX_JS, "text/javascript", CACHE_CONTROL_REVALIDATE)
}

pub async fn legacy_katex_mhchem_js_handler() -> Response {
    serve(KATEX_MHCHEM_JS, "text/javascript", CACHE_CONTROL_REVALIDATE)
}

pub async fn legacy_katex_font_handler(Path(name): Path<String>) -> Response {
    match katex_font_bytes(&name) {
        Some(bytes) => serve(bytes, "font/woff2", CACHE_CONTROL_REVALIDATE),
        None => not_found(),
    }
}

/// WOFF2 only: it is the smallest format and every browser that can run the
/// rest of the page supports it, so the `.ttf` and `.woff` faces the
/// stylesheet also lists are deliberately absent.
fn katex_font_bytes(name: &str) -> Option<&'static [u8]> {
    match name {
        "KaTeX_AMS-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_AMS-Regular.woff2"
        )),
        "KaTeX_Caligraphic-Bold.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Caligraphic-Bold.woff2"
        )),
        "KaTeX_Caligraphic-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Caligraphic-Regular.woff2"
        )),
        "KaTeX_Fraktur-Bold.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Fraktur-Bold.woff2"
        )),
        "KaTeX_Fraktur-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Fraktur-Regular.woff2"
        )),
        "KaTeX_Main-Bold.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Main-Bold.woff2"
        )),
        "KaTeX_Main-BoldItalic.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Main-BoldItalic.woff2"
        )),
        "KaTeX_Main-Italic.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Main-Italic.woff2"
        )),
        "KaTeX_Main-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Main-Regular.woff2"
        )),
        "KaTeX_Math-BoldItalic.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Math-BoldItalic.woff2"
        )),
        "KaTeX_Math-Italic.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Math-Italic.woff2"
        )),
        "KaTeX_SansSerif-Bold.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_SansSerif-Bold.woff2"
        )),
        "KaTeX_SansSerif-Italic.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_SansSerif-Italic.woff2"
        )),
        "KaTeX_SansSerif-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_SansSerif-Regular.woff2"
        )),
        "KaTeX_Script-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Script-Regular.woff2"
        )),
        "KaTeX_Size1-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Size1-Regular.woff2"
        )),
        "KaTeX_Size2-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Size2-Regular.woff2"
        )),
        "KaTeX_Size3-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Size3-Regular.woff2"
        )),
        "KaTeX_Size4-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Size4-Regular.woff2"
        )),
        "KaTeX_Typewriter-Regular.woff2" => Some(include_bytes!(
            "../../../vendor/katex/fonts/KaTeX_Typewriter-Regular.woff2"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::KATEX_CSS;
    use super::KATEX_CSS_URL;
    use super::KATEX_FONT_DIR;
    use super::KATEX_REV;

    /// The stylesheet is served `immutable` under its revision, so every URL
    /// it names has to carry that revision too — otherwise a KaTeX bump
    /// leaves clients pairing new glyph metrics with week-old font files.
    #[test]
    fn the_stylesheet_asks_for_revisioned_fonts() {
        let want = format!("url(/katex/fonts/{}/", *KATEX_REV);
        assert!(KATEX_CSS.contains(&want), "no revisioned font reference");
        assert!(
            !KATEX_CSS.contains(&format!("{KATEX_FONT_DIR}KaTeX_")),
            "a font reference was left at the unrevisioned path"
        );
    }

    #[test]
    fn the_asset_urls_carry_the_revision() {
        assert_eq!(*KATEX_CSS_URL, format!("/katex/{}/katex.css", *KATEX_REV));
    }
}
