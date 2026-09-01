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

//! Termination signals for the drill and serve servers.
//!
//! Installing a handler replaces the process' default disposition for that
//! signal, so once the first Ctrl+C is caught the kernel no longer kills the
//! process on the next one. If graceful shutdown then blocks (a client
//! connection that never closes), the user would be stuck with no way out
//! short of SIGKILL. To avoid that, catching the first signal arms an escape
//! hatch: the next signal exits immediately.

use std::future::Future;
use std::future::pending;

use tokio::select;
use tokio::signal;

/// Exit status for a process killed by SIGINT, by shell convention
/// (128 + SIGINT).
const SIGINT_EXIT_CODE: i32 = 130;

/// Resolve once a termination signal (SIGINT, or SIGTERM on Unix) arrives,
/// then arm the escape hatch so a second one exits the process immediately.
///
/// Reviews are written to the database as they happen, so a forced exit
/// loses no grades; it only leaves the session row open.
pub async fn terminate_signal() {
    termination().await;
    tokio::spawn(force_exit_on(termination(), || {
        std::process::exit(SIGINT_EXIT_CODE)
    }));
}

/// Run `exit` once `next` resolves. Factored out of [`terminate_signal`] so
/// the escape hatch can be tested without signalling the test process.
async fn force_exit_on(next: impl Future<Output = ()>, exit: impl FnOnce()) {
    next.await;
    exit();
}

/// Resolve on the first SIGINT or SIGTERM. If no handler can be installed,
/// never resolve: the default disposition still terminates the process, so
/// the user is not left without a way to stop it.
async fn termination() {
    let interrupt = async {
        if let Err(e) = signal::ctrl_c().await {
            log::error!(
                "Failed to install Ctrl+C handler; graceful shutdown on Ctrl+C is disabled: {e}"
            );
            pending::<()>().await;
        }
    };

    select! {
        _ = interrupt => log::debug!("Received Ctrl+C"),
        _ = sigterm() => log::debug!("Received SIGTERM"),
    }
}

/// Resolve on SIGTERM, so a supervisor stopping the process still closes the
/// session row cleanly.
#[cfg(unix)]
async fn sigterm() {
    use tokio::signal::unix::SignalKind;
    use tokio::signal::unix::signal as unix_signal;

    match unix_signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(e) => {
            log::error!(
                "Failed to install SIGTERM handler; graceful shutdown on SIGTERM is disabled: {e}"
            );
            pending::<()>().await;
        }
    }
}

#[cfg(not(unix))]
async fn sigterm() {
    pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    use tokio::sync::oneshot::channel;

    /// BUG-52: catching the first termination signal disables the default
    /// disposition, so a second one must still stop the process.
    #[tokio::test]
    async fn test_second_signal_forces_exit() {
        let exited = Arc::new(AtomicBool::new(false));
        let flag = exited.clone();
        let (tx, rx) = channel::<()>();
        let hatch = tokio::spawn(force_exit_on(
            async {
                rx.await.ok();
            },
            move || flag.store(true, Ordering::SeqCst),
        ));

        assert!(!exited.load(Ordering::SeqCst), "exited before any signal");

        let _ = tx.send(());
        hatch.await.expect("escape hatch task panicked");

        assert!(exited.load(Ordering::SeqCst), "second signal did not exit");
    }
}
