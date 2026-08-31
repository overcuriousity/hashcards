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

pub mod cache;
pub mod get;
pub mod hljs;
pub mod katex;
pub mod post;
pub mod server;
pub mod state;
pub mod stats;
pub mod template;

#[cfg(test)]
mod tests {
    use std::fs::create_dir_all;

    use portpicker::pick_unused_port;
    use reqwest::StatusCode;
    use tempfile::tempdir;
    use tokio::spawn;

    use crate::cmd::drill::server::AnswerControls;
    use crate::cmd::drill::server::ServerConfig;
    use crate::cmd::drill::server::start_server;
    use crate::error::Fallible;
    use crate::helper::create_tmp_copy_of_test_directory;
    use crate::types::performance::Jitter;
    use crate::types::timestamp::Timestamp;
    use crate::utils::wait_for_server;

    const TEST_HOST: &str = "127.0.0.1";

    #[tokio::test]
    async fn test_start_server_on_non_existent_directory() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some("./derpherp".to_string()),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        let result = start_server(config).await;
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.to_string(), "error: directory does not exist.");
        Ok(())
    }

    #[tokio::test]
    async fn test_start_server_with_no_cards_due() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?.path().to_path_buf().canonicalize()?;
        create_dir_all(&dir)?;
        let session_started_at = Timestamp::now();
        let dir = dir.canonicalize().unwrap().display().to_string();
        let config = ServerConfig {
            directory: Some(dir),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        start_server(config).await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_e2e() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // Hit the `style.css` endpoint.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/style.css")).await?;
        assert!(response.status().is_success());
        assert_eq!(response.headers().get("content-type").unwrap(), "text/css");

        // Hit the `script.js` endpoint.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/script.js")).await?;
        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/javascript"
        );

        // Hit the not found endpoint.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/herp-derp")).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Hit the file endpoint.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/file/foo.jpg")).await?;
        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "image/jpeg"
        );

        // Hit the file endpoint with a non-existent file.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/file/foo.png")).await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Hit the root endpoint.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/")).await?;
        assert!(response.status().is_success());
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        // The collection has one basic and one cloze card. Their order in the
        // queue follows the hash sort in parse_deck, so drive both cards and
        // assert on what the whole session showed rather than on the order.
        let mut fronts = vec![response.text().await?];
        let mut backs: Vec<String> = Vec::new();
        let mut completion: Option<String> = None;
        for card in 0..2 {
            // Hit reveal.
            let response = reqwest::Client::new()
                .post(format!("http://{TEST_HOST}:{port}/"))
                .form(&[("action", "Reveal")])
                .send()
                .await?;
            assert!(response.status().is_success());
            backs.push(response.text().await?);

            // Hit 'Good'.
            let response = reqwest::Client::new()
                .post(format!("http://{TEST_HOST}:{port}/"))
                .form(&[("action", "Good")])
                .send()
                .await?;
            assert!(response.status().is_success());
            let html = response.text().await?;
            if card == 0 {
                fronts.push(html);
            } else {
                completion = Some(html);
            }
        }

        // Both cards were shown, front and back, in some order.
        assert!(
            fronts
                .iter()
                .any(|h| h.contains("baz <span class='cloze'>.............</span>")),
            "the cloze card's front was never shown: {fronts:?}"
        );
        assert!(
            fronts.iter().any(|h| h.contains("FOO")),
            "the basic card's front was never shown: {fronts:?}"
        );
        assert!(
            backs
                .iter()
                .any(|h| h.contains("baz <span class='cloze-reveal'>quux</span>")),
            "the cloze card's back was never shown: {backs:?}"
        );
        assert!(
            backs.iter().any(|h| h.contains("BAR")),
            "the basic card's back was never shown: {backs:?}"
        );
        // Grading the second card finishes the session.
        let completion = completion.expect("the loop ran twice");
        assert!(completion.contains("Session Completed"), "{completion}");

        Ok(())
    }

    #[tokio::test]
    async fn test_undo() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // The first card of the session; which one it is follows the hash
        // sort in parse_deck, so capture it instead of assuming.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/")).await?;
        assert!(response.status().is_success());
        let first_card = response.text().await?;

        // Hit reveal.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Reveal")])
            .send()
            .await?;
        assert!(response.status().is_success());

        // Hit 'Good'.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Good")])
            .send()
            .await?;
        assert!(response.status().is_success());

        // Hit undo.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Undo")])
            .send()
            .await?;
        assert!(response.status().is_success());
        let html = response.text().await?;
        // Undo puts the graded card back at the head of the queue.
        assert_eq!(html, first_card);

        Ok(())
    }

    #[tokio::test]
    async fn test_undo_initial() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // Hit undo.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Undo")])
            .send()
            .await?;
        assert!(response.status().is_success());

        Ok(())
    }

    #[tokio::test]
    async fn test_answer_without_reveal() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // Hit 'Hard'.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Hard")])
            .send()
            .await?;
        assert!(response.status().is_success());

        Ok(())
    }

    #[tokio::test]
    async fn test_undo_forgetting() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // The first card of the session; which one it is follows the hash
        // sort in parse_deck, so capture it instead of assuming.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/")).await?;
        assert!(response.status().is_success());
        let first_card = response.text().await?;

        // Hit reveal.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Reveal")])
            .send()
            .await?;
        assert!(response.status().is_success());

        // Hit 'Forgot'.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Forgot")])
            .send()
            .await?;
        assert!(response.status().is_success());

        // Hit undo.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "Undo")])
            .send()
            .await?;
        assert!(response.status().is_success());
        let html = response.text().await?;
        // Undo puts the graded card back at the head of the queue.
        assert_eq!(html, first_card);

        Ok(())
    }

    #[tokio::test]
    async fn test_end() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // Hit end.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/"))
            .form(&[("action", "End")])
            .send()
            .await?;
        assert!(response.status().is_success());
        let html = response.text().await?;
        assert!(html.contains("Session Ended"));

        Ok(())
    }

    #[tokio::test]
    async fn test_flash_query_param_renders_banner() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let session_started_at = Timestamp::now();
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at,
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        spawn(async move { start_server(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let response = reqwest::get(format!(
            "http://{TEST_HOST}:{port}/?flash=Hello%20there&kind=success"
        ))
        .await?;
        assert!(response.status().is_success());
        let body = response.text().await?;
        assert!(body.contains("flash-success"), "body: {body}");
        assert!(body.contains("Hello there"));
        Ok(())
    }

    /// FEAT-02: the drill server serves the stats page at /stats.
    #[tokio::test]
    async fn test_stats_page() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let directory = create_tmp_copy_of_test_directory()?;
        let config = ServerConfig {
            directory: Some(directory),
            host: TEST_HOST.to_string(),
            port,
            session_started_at: Timestamp::now(),
            card_limit: None,
            new_card_limit: None,
            deck_filter: None,
            shuffle: false,
            jitter: Jitter::none(),
            answer_controls: AnswerControls::Full,
            bury_siblings: false,
        };
        let handle = spawn(start_server(config));
        wait_for_server(TEST_HOST, port).await?;
        let resp = reqwest::get(format!("http://{TEST_HOST}:{port}/stats")).await?;
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body = resp.text().await?;
        assert!(
            body.contains("Due forecast"),
            "missing forecast section: {body}"
        );
        assert!(
            body.contains("Reviews per day"),
            "missing history section: {body}"
        );
        assert!(
            body.contains("Grade distribution"),
            "missing grades section: {body}"
        );
        handle.abort();
        Ok(())
    }
}
