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




#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use tokio::net::TcpStream;
#[cfg(test)]
use tokio::time::sleep;

#[cfg(test)]
use crate::error::Fallible;

// max-age is one week in seconds.
pub const CACHE_CONTROL_IMMUTABLE: &str = "public, max-age=604800, immutable";

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
