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

pub mod check;
pub mod drill;
pub mod export;
pub mod orphans;
pub mod serve;
pub mod signals;
pub mod stats;
pub mod stats_page;

use crate::error::ErrorReport;
use crate::error::Fallible;

/// Run a blocking closure on tokio's blocking pool.
///
/// Command handlers routinely parse decks, validate media and talk to SQLite.
/// Doing that directly inside an async handler blocks a tokio worker for the
/// whole operation, so it belongs here instead.
pub async fn run_blocking<T, F>(f: F) -> Fallible<T>
where
    F: FnOnce() -> Fallible<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorReport::new(format!("Internal error: a background task failed: {e}")))?
}
