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

//! The two typefaces the stylesheet names, served from the binary.
//!
//! `style.css` asks for `/fonts/{rev}/{name}`; only the four names below
//! exist, so the path is matched rather than resolved and nothing here
//! touches the filesystem. The revision is a hash of all four faces
//! together: they ship as one set, and the stylesheet that names them is
//! rewritten to carry it (see `template::STYLE_CSS`), so replacing a font
//! changes both the font's URL and the stylesheet's.

use std::sync::LazyLock;

use axum::extract::Path;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::http::header::CONTENT_TYPE;

use crate::utils::CACHE_CONTROL_REVALIDATE;
use crate::utils::revision;
use crate::utils::revisioned_cache_control;

const INTER_400: &[u8] = include_bytes!("../../../vendor/fonts/inter-400.woff2");
const INTER_500: &[u8] = include_bytes!("../../../vendor/fonts/inter-500.woff2");
const INTER_600: &[u8] = include_bytes!("../../../vendor/fonts/inter-600.woff2");
const JETBRAINS_MONO_400: &[u8] = include_bytes!("../../../vendor/fonts/jetbrains-mono-400.woff2");

/// The path prefix `style.css` is rewritten to name, revision included.
pub static FONT_DIR_URL: LazyLock<String> = LazyLock::new(|| format!("/fonts/{}", *FONT_REV));

pub static FONT_REV: LazyLock<String> =
    LazyLock::new(|| revision(&[INTER_400, INTER_500, INTER_600, JETBRAINS_MONO_400]));

fn font_bytes(name: &str) -> Option<&'static [u8]> {
    match name {
        "inter-400.woff2" => Some(INTER_400),
        "inter-500.woff2" => Some(INTER_500),
        "inter-600.woff2" => Some(INTER_600),
        "jetbrains-mono-400.woff2" => Some(JETBRAINS_MONO_400),
        _ => None,
    }
}

fn serve(name: &str, cache_control: &'static str) -> Response {
    match font_bytes(name) {
        Some(bytes) => (
            StatusCode::OK,
            [(CONTENT_TYPE, "font/woff2"), (CACHE_CONTROL, cache_control)],
            bytes,
        ),
        None => (
            StatusCode::NOT_FOUND,
            [
                (CONTENT_TYPE, "text/plain"),
                (CACHE_CONTROL, CACHE_CONTROL_REVALIDATE),
            ],
            b"Not Found" as &'static [u8],
        ),
    }
}

type Response = (StatusCode, [(HeaderName, &'static str); 2], &'static [u8]);

pub async fn font_handler(Path((rev, name)): Path<(String, String)>) -> Response {
    serve(&name, revisioned_cache_control(&rev, &FONT_REV))
}

/// The old unrevisioned path. A page rendered before the fonts were named by
/// their contents still asks for it, and a page in the wrong typeface is
/// worse than one that revalidates.
pub async fn legacy_font_handler(Path(name): Path<String>) -> Response {
    serve(&name, CACHE_CONTROL_REVALIDATE)
}

#[cfg(test)]
mod tests {
    use super::FONT_DIR_URL;
    use super::FONT_REV;
    use crate::utils::CACHE_CONTROL_IMMUTABLE;
    use crate::utils::CACHE_CONTROL_REVALIDATE;
    use crate::utils::revisioned_cache_control;

    #[test]
    fn the_font_directory_carries_the_revision() {
        assert_eq!(*FONT_DIR_URL, format!("/fonts/{}", *FONT_REV));
    }

    /// A request from HTML older than the running build must be answered
    /// with the current font, but must not be allowed to keep it: at the
    /// wrong revision the path no longer names what is served.
    #[test]
    fn a_stale_revision_may_not_be_cached() {
        assert_eq!(
            revisioned_cache_control(&FONT_REV, &FONT_REV),
            CACHE_CONTROL_IMMUTABLE
        );
        assert_eq!(
            revisioned_cache_control("0000000000000000", &FONT_REV),
            CACHE_CONTROL_REVALIDATE
        );
    }
}
