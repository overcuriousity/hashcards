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
//! `style.css` asks for `/fonts/{name}`; only the four names below exist, so
//! the path is matched rather than resolved and nothing here touches the
//! filesystem.

use axum::extract::Path;
use axum::http::HeaderName;
use axum::http::StatusCode;
use axum::http::header::CACHE_CONTROL;
use axum::http::header::CONTENT_TYPE;

use crate::utils::CACHE_CONTROL_IMMUTABLE;

pub async fn font_handler(
    Path(name): Path<String>,
) -> (StatusCode, [(HeaderName, &'static str); 2], &'static [u8]) {
    let bytes: &'static [u8] = match name.as_str() {
        "inter-400.woff2" => include_bytes!("../../../vendor/fonts/inter-400.woff2"),
        "inter-500.woff2" => include_bytes!("../../../vendor/fonts/inter-500.woff2"),
        "inter-600.woff2" => include_bytes!("../../../vendor/fonts/inter-600.woff2"),
        "jetbrains-mono-400.woff2" => {
            include_bytes!("../../../vendor/fonts/jetbrains-mono-400.woff2")
        }
        _ => {
            return (
                StatusCode::NOT_FOUND,
                [(CONTENT_TYPE, "text/plain"), (CACHE_CONTROL, "no-cache")],
                b"Not Found",
            );
        }
    };
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, "font/woff2"),
            (CACHE_CONTROL, CACHE_CONTROL_IMMUTABLE),
        ],
        bytes,
    )
}
