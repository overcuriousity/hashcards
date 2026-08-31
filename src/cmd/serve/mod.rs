mod bookmarks;
mod browse;
pub mod config;
mod edit;
mod git;
mod handlers;
mod hedgedoc;
mod hedgedoc_ui;
mod href;
mod landing;
pub mod server;
mod state;

#[cfg(test)]
mod tests {
    use std::fs::write;
    use std::path::PathBuf;

    use portpicker::pick_unused_port;
    use tempfile::tempdir;
    use tokio::spawn;

    use crate::cmd::serve::config::DefaultsSection;
    use crate::cmd::serve::config::ResolvedCollection;
    use crate::cmd::serve::config::ResolvedServeConfig;
    use crate::cmd::serve::server::start_serve;
    use crate::error::Fallible;
    use crate::utils::wait_for_server;

    const TEST_HOST: &str = "127.0.0.1";

    /// Start a serve-mode server for one collection rooted at `coll_dir`,
    /// registered under `slug`. Returns the port.
    async fn spawn_test_server(coll_dir: PathBuf, slug: &str) -> Fallible<u16> {
        let port = pick_unused_port().unwrap();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Test Collection".to_string(),
                slug: slug.to_string(),
                coll_dir: coll_dir.clone(),
                db_path: coll_dir.join("hashcards.db"),
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            _temp_dir: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;
        Ok(port)
    }

    #[tokio::test]
    async fn test_flash_query_param_renders_on_collection_page() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let slug = "test-collection";
        let port = spawn_test_server(coll_dir, slug).await?;

        let response = reqwest::get(format!(
            "http://{TEST_HOST}:{port}/collection/{slug}?flash=Hello%20world&kind=success"
        ))
        .await?;
        let body = response.text().await?;
        assert!(body.contains("flash-success"), "body: {body}");
        assert!(body.contains("Hello world"));
        Ok(())
    }

    /// Regression test: POSTing multiple `decks` values to /collection/{slug}/start
    /// must not fail with "duplicate field `decks`".
    #[tokio::test]
    async fn test_start_with_multiple_decks() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();

        // Create two markdown files representing two different decks.
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        write(coll_dir.join("Beta.md"), "Q: What is 2+2?\nA: 4\n")?;

        let slug = "test-collection".to_string();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Test Collection".to_string(),
                slug: slug.clone(),
                coll_dir: coll_dir.clone(),
                db_path: coll_dir.join("hashcards.db"),
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            _temp_dir: None,
        };

        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // POST with multiple `decks` values — this used to fail with
        // "Failed to deserialize form body: duplicate field `decks`".
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/collection/{slug}/start"))
            .body("decks=Alpha&decks=Beta")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;

        // The handler redirects on success; reqwest follows redirects by
        // default, so any 2xx status means the form was accepted.
        assert!(
            response.status().is_success(),
            "expected success, got {}",
            response.status()
        );

        // The redirect target must show the running drill session, not the
        // deck browser (the redirect alone fires on success and failure).
        let body = response.text().await?;
        assert!(
            body.contains("value=\"Reveal\""),
            "expected the post-redirect page to show the drill session, got: {body}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_start_with_no_decks_is_rejected_with_flash() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let slug = "test-collection";
        let port = spawn_test_server(coll_dir, slug).await?;

        // POST with no `decks` field at all (no-JS or hand-made form).
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/collection/{slug}/start"))
            .body("")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        let body = response.text().await?;
        // The post-redirect page shows the flash and stays on the deck browser:
        assert!(body.contains("Select at least one deck"), "body: {body}");
        assert!(body.contains("flash-error"));
        // No session was started (a session page would show the Reveal button).
        assert!(!body.contains("value=\"Reveal\""));
        Ok(())
    }

    #[tokio::test]
    async fn test_bookmark_delete_error_is_surfaced_as_flash() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let slug = "test-collection";
        let port = spawn_test_server(coll_dir, slug).await?;

        // "nothex" is not a valid card hash: the delete must fail, and the
        // failure must be visible on the post-redirect bookmarks page.
        let response = reqwest::Client::new()
            .post(format!(
                "http://{TEST_HOST}:{port}/collection/{slug}/bookmarks/nothex/delete"
            ))
            .send()
            .await?;
        let body = response.text().await?;
        assert!(body.contains("flash-error"), "body: {body}");
        Ok(())
    }

    #[tokio::test]
    async fn test_hedgedoc_add_empty_url_is_surfaced_as_flash() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let port = spawn_test_server(coll_dir, "test-collection").await?;

        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/hedgedoc/add"))
            .body("url=")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        let body = response.text().await?;
        assert!(body.contains("Enter a HedgeDoc URL"), "body: {body}");
        assert!(body.contains("flash-error"));
        Ok(())
    }

    /// Regression test (BUG-01): a request error mid-session must not drop
    /// the drill session. Forces a render error by deleting the card's
    /// source file, then asserts the session survives and the next GET
    /// renders the same card rather than the deck browser.
    #[tokio::test]
    async fn test_session_survives_render_error() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        let card_file = coll_dir.join("Alpha.md");
        write(&card_file, "Q: What is 1+1?\nA: 2\n")?;

        let slug = "test-collection".to_string();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Test Collection".to_string(),
                slug: slug.clone(),
                coll_dir: coll_dir.clone(),
                db_path: coll_dir.join("hashcards.db"),
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            _temp_dir: None,
        };

        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;
        let client = reqwest::Client::new();

        // Start a drill session; the redirect is followed to the session page.
        let response = client
            .post(format!("http://{TEST_HOST}:{port}/collection/{slug}/start"))
            .body("decks=Alpha")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        assert!(response.status().is_success());
        let body = response.text().await?;
        assert!(
            body.contains("progress-bar"),
            "expected a running session, got: {body}"
        );

        // Force a render error mid-session: the card's source file vanishes.
        std::fs::remove_file(&card_file)?;
        let response = client
            .get(format!("http://{TEST_HOST}:{port}/collection/{slug}"))
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::INTERNAL_SERVER_ERROR
        );

        // Restore the file (identical content, identical hash). The session
        // must have survived the error: the next GET renders the same card,
        // not the deck browser.
        write(&card_file, "Q: What is 1+1?\nA: 2\n")?;
        let response = client
            .get(format!("http://{TEST_HOST}:{port}/collection/{slug}"))
            .send()
            .await?;
        assert!(response.status().is_success());
        let body = response.text().await?;
        assert!(
            body.contains("progress-bar"),
            "session was dropped by the render error: {body}"
        );
        assert!(
            !body.contains("deck-tree"),
            "deck browser rendered instead of the surviving session"
        );
        Ok(())
    }
}
