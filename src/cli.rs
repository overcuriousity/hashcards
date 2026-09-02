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

use std::fs::canonicalize;
use std::path::Path;
use std::path::PathBuf;

use clap::Parser;

use crate::cmd::serve::config::ResolvedServeConfig;
use crate::cmd::serve::config::load_config;
use crate::cmd::serve::server::start_serve;
use crate::error::Fallible;
use crate::error::fail;

/// The configuration file used when `--config` is not given.
const DEFAULT_CONFIG_FILE: &str = "hashcards.toml";

#[derive(Parser)]
#[command(
    version,
    about = "A web server for plain text spaced repetition.",
    after_help = "\
The bind address, the collections to serve, and everything else are read
from the configuration file. See hashcards.example.toml for the format."
)]
struct Cli {
    /// Path to the configuration file. Defaults to hashcards.toml in the
    /// current directory.
    #[arg(long)]
    config: Option<String>,
}

pub async fn entrypoint() -> Fallible<()> {
    let cli = Cli::parse();
    let resolved = resolve_config(cli.config.as_deref())?;
    start_serve(resolved).await
}

/// Locate and load the configuration file.
///
/// Unlike the pre-fork CLI, there is no way to run without one: the
/// config file is what declares the collections, their owners, and the
/// OIDC settings that gate them.
fn resolve_config(config: Option<&str>) -> Fallible<ResolvedServeConfig> {
    resolve_config_at(config, Path::new(DEFAULT_CONFIG_FILE))
}

/// `resolve_config` with the fallback path injected, so the no-`--config`
/// branch can be tested without depending on the process's working
/// directory (which tests share, and cannot change without racing).
fn resolve_config_at(config: Option<&str>, default_path: &Path) -> Fallible<ResolvedServeConfig> {
    let path: PathBuf = match config {
        Some(path) => PathBuf::from(path),
        None => default_path.to_path_buf(),
    };

    if !path.exists() {
        return match config {
            Some(path) => fail(format!("Config file not found: {path}")),
            None => fail(format!(
                "No configuration file found. Expected {DEFAULT_CONFIG_FILE} in the \
                 current directory, or a path given with --config. See \
                 hashcards.example.toml for the format."
            )),
        };
    }

    let canonical = canonicalize(&path)
        .map_err(|e| crate::error::ErrorReport::new(format!("Failed to resolve {path:?}: {e}")))?;
    let config = load_config(Path::new(&path))?;
    Ok(ResolvedServeConfig::from_toml(config)?.with_config_path(canonical))
}

#[cfg(test)]
mod tests {
    use std::fs::write;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn test_missing_explicit_config_is_an_error() {
        let e = resolve_config(Some("./no-such-config.toml"))
            .err()
            .expect("expected an error for a config path that does not exist");
        assert!(
            e.to_string().contains("no-such-config.toml"),
            "the error must name the missing file, got: {e}"
        );
    }

    /// Running with no config at all used to start an unauthenticated
    /// HedgeDoc-only server in a temp directory. It must now fail loudly,
    /// naming the file it looked for.
    #[test]
    fn test_missing_default_config_is_an_error() -> Fallible<()> {
        let dir = tempdir()?;
        let missing = dir.path().join(DEFAULT_CONFIG_FILE);
        let e = resolve_config_at(None, &missing)
            .err()
            .expect("expected an error when the default config is absent");
        let message = e.to_string();
        assert!(
            message.contains("No configuration file found"),
            "the no-config branch must report its own error, got: {message}"
        );
        assert!(
            message.contains(DEFAULT_CONFIG_FILE),
            "the error must name the file it looked for, got: {message}"
        );
        Ok(())
    }

    /// The default config is loaded when it does exist, rather than the
    /// absence of `--config` being an error in itself.
    #[test]
    fn test_default_config_is_loaded_when_present() -> Fallible<()> {
        let dir = tempdir()?;
        let data_dir = dir.path().join("data");
        let config_path = dir.path().join(DEFAULT_CONFIG_FILE);
        write(
            &config_path,
            format!(
                "[server]\ndata_dir = {:?}\n",
                data_dir.to_str().expect("temp path must be valid UTF-8")
            ),
        )?;

        let resolved = resolve_config_at(None, &config_path)?;
        assert!(resolved.config_path.is_some());
        Ok(())
    }

    #[test]
    fn test_config_is_loaded_and_its_path_recorded() -> Fallible<()> {
        let dir = tempdir()?;
        let data_dir = dir.path().join("data");
        let config_path = dir.path().join(DEFAULT_CONFIG_FILE);
        write(
            &config_path,
            format!(
                "[server]\ndata_dir = {:?}\n",
                data_dir.to_str().expect("temp path must be valid UTF-8")
            ),
        )?;

        let resolved = resolve_config(Some(
            config_path.to_str().expect("temp path must be valid UTF-8"),
        ))?;
        assert!(
            resolved.config_path.is_some(),
            "the resolved config must record where it was loaded from"
        );
        Ok(())
    }
}
