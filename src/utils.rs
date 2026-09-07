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

use std::fs::create_dir_all;
use std::path::Path;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use tokio::net::TcpStream;
#[cfg(test)]
use tokio::time::sleep;

use crate::error::ErrorReport;
use crate::error::Fallible;

// max-age is one week in seconds.
pub const CACHE_CONTROL_IMMUTABLE: &str = "public, max-age=604800, immutable";

/// For assets served from a path that does not name their contents. A cache
/// may hold them, but must ask before reusing them: at a fixed path there is
/// nothing else to tell a client its copy has gone stale.
pub const CACHE_CONTROL_REVALIDATE: &str = "no-cache";

/// Sixteen hex characters naming this build's copy of an asset.
///
/// Short enough to read in a URL, long enough that two builds of the same
/// asset never collide. Several parts may be hashed together when they are
/// only ever shipped as a set — the KaTeX stylesheet, script and fonts are
/// one vendored package and change as one.
pub fn revision(parts: &[&[u8]]) -> String {
    let mut hasher = blake3::Hasher::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().to_hex()[..16].to_string()
}

/// What a client asking for `requested` may do with the answer.
///
/// `immutable` does not merely permit a cache to skip revalidation, it
/// forbids revalidation, so it is only honest when the path names the bytes.
/// A request carrying this build's revision gets that promise. One carrying
/// a stale revision comes from HTML rendered by an older build — it is
/// served the current bytes, so the page still works, but it may not keep
/// them: the next load will ask for the right revision anyway.
pub fn revisioned_cache_control(requested: &str, current: &str) -> &'static str {
    if requested == current {
        CACHE_CONTROL_IMMUTABLE
    } else {
        CACHE_CONTROL_REVALIDATE
    }
}

/// `create_dir_all`, but the error says which directory and why.
///
/// `std::io::Error` carries no path, so the bare `?` on a `create_dir_all`
/// produces "Permission denied (os error 13)" and leaves the operator to
/// guess which of the server's directories it meant. `what` names the
/// directory's role, so the message reads as a sentence.
pub fn ensure_dir(path: &Path, what: &str) -> Fallible<()> {
    create_dir_all(path).map_err(|e| {
        ErrorReport::new(format!(
            "failed to create the {what} at {}: {e}. Create it and make it writable by the              user the server runs as, or point the configuration somewhere that already is.",
            path.display()
        ))
    })
}

/// Block until `host:port` accepts a connection. Test-only: nothing in the
/// server waits on itself.
#[cfg(test)]
pub async fn wait_for_server(host: &str, port: u16) -> Fallible<()> {
    loop {
        if let Ok(stream) = TcpStream::connect(format!("{host}:{port}")).await {
            drop(stream);
            break;
        }
        sleep(Duration::from_millis(1)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::fs::Permissions;
    #[cfg(unix)]
    use std::fs::set_permissions;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_ensure_dir_creates_nested_directories() -> Fallible<()> {
        let dir = tempdir()?;
        let nested = dir.path().join("data").join("db");
        ensure_dir(&nested, "review database directory")?;
        assert!(nested.is_dir(), "the directory must exist afterwards");
        // Creating one that already exists is not an error.
        ensure_dir(&nested, "review database directory")?;
        Ok(())
    }

    /// A bare `create_dir_all(..)?` produced "Permission denied (os error 13)"
    /// and left the operator to guess which directory it meant. The message
    /// must name the path and say what to do about it.
    ///
    /// Unix-only: the mode bits that make a directory unwritable have no
    /// Windows equivalent, and `create_dir_all` there would simply succeed.
    #[cfg(unix)]
    #[test]
    fn test_ensure_dir_error_names_the_path() -> Fallible<()> {
        let dir = tempdir()?;
        let readonly = dir.path().join("readonly");
        std::fs::create_dir(&readonly)?;
        set_permissions(&readonly, Permissions::from_mode(0o500))?;

        let target = readonly.join("data");
        let message = match ensure_dir(&target, "data directory") {
            Ok(()) => {
                // Running as root ignores the mode bits entirely.
                set_permissions(&readonly, Permissions::from_mode(0o700))?;
                return Ok(());
            }
            Err(e) => e.to_string(),
        };

        // Restore before the assertions, so a failure still cleans up.
        set_permissions(&readonly, Permissions::from_mode(0o700))?;
        assert!(
            message.contains(&target.display().to_string()),
            "the error must name the directory it could not create: {message}"
        );
        assert!(
            message.contains("data directory"),
            "the error must say which directory this is: {message}"
        );
        assert!(
            message.contains("writable"),
            "the error must say what to do about it: {message}"
        );
        Ok(())
    }
}
