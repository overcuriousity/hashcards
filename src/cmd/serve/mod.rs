mod auth;
mod bookmarks;
mod browse;
pub mod config;
mod decks;
mod edit;
pub mod export;
mod files;
mod files_ui;
mod git;
mod handlers;
mod hedgedoc;
mod hedgedoc_ui;
mod href;
mod landing;
mod local;
pub mod server;
mod source;
mod state;
mod upload;

pub mod stats;

#[cfg(test)]
mod tests {
    use std::fs::read_to_string;
    use std::fs::write;
    use std::path::PathBuf;

    use portpicker::pick_unused_port;
    use tempfile::tempdir;
    use tokio::spawn;

    use crate::cmd::serve::config::DefaultsSection;
    use crate::cmd::serve::config::ResolvedCollection;
    use crate::cmd::serve::config::ResolvedServeConfig;
    use crate::cmd::serve::server::start_serve;
    use crate::db::Database;
    use crate::error::Fallible;
    use crate::types::timestamp::Timestamp;
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
                owner: None,
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
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

    /// The theme switch is three pieces that have to agree: the stylesheet's
    /// two override selectors, the inline script that applies a stored choice
    /// before the first paint, and the button `script.js` looks for. Any one
    /// of them renamed alone leaves a switch that does nothing.
    #[tokio::test]
    async fn test_theme_switch_is_wired_end_to_end() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let port = spawn_test_server(coll_dir, "test-collection").await?;

        let css = reqwest::get(format!("http://{TEST_HOST}:{port}/style.css"))
            .await?
            .text()
            .await?;
        assert!(
            css.contains(r#":root[data-theme="dark"]"#),
            "no dark override"
        );
        assert!(
            css.contains(r#":root:not([data-theme="light"])"#),
            "no light opt-out"
        );

        let page = reqwest::get(format!("http://{TEST_HOST}:{port}/"))
            .await?
            .text()
            .await?;
        assert!(page.contains("hashcards.theme"), "no pre-paint script");
        assert!(page.contains("data-theme-toggle"), "no toggle button");
        assert!(page.contains("data-theme-label"), "no toggle label");

        let js = reqwest::get(format!("http://{TEST_HOST}:{port}/script.js"))
            .await?
            .text()
            .await?;
        assert!(
            js.contains("data-theme-toggle"),
            "script.js does not bind the toggle"
        );
        assert!(
            js.contains("hashcards.theme"),
            "script.js stores under another key"
        );
        Ok(())
    }

    /// The stylesheet names four font files and serves them from the binary.
    /// A typo in either the route or a filename is invisible in the browser —
    /// the page simply falls back to a system font — so the names are checked
    /// here instead.
    #[tokio::test]
    async fn test_fonts_are_served() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let port = spawn_test_server(coll_dir, "test-collection").await?;

        let css = reqwest::get(format!("http://{TEST_HOST}:{port}/style.css"))
            .await?
            .text()
            .await?;

        for name in [
            "inter-400.woff2",
            "inter-500.woff2",
            "inter-600.woff2",
            "jetbrains-mono-400.woff2",
        ] {
            assert!(
                css.contains(&format!("/fonts/{name}")),
                "{name} not in style.css"
            );
            let response = reqwest::get(format!("http://{TEST_HOST}:{port}/fonts/{name}")).await?;
            assert!(
                response.status().is_success(),
                "{name}: {:?}",
                response.status()
            );
            assert_eq!(
                response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok()),
                Some("font/woff2"),
                "{name}"
            );
            assert!(!response.bytes().await?.is_empty(), "{name} is empty");
        }

        // A name that is not one of the four is a 404, not a path to read.
        let response =
            reqwest::get(format!("http://{TEST_HOST}:{port}/fonts/..%2Fstyle.css")).await?;
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
        Ok(())
    }

    /// Regression: with no [oidc] configured, /auth/login must not exist —
    /// proves the auth routes and middleware are opt-in, not always-on.
    #[tokio::test]
    async fn test_auth_routes_absent_without_oidc_config() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        let slug = "test-collection";
        let port = spawn_test_server(coll_dir, slug).await?;

        let response = reqwest::Client::new()
            .get(format!("http://{TEST_HOST}:{port}/auth/login"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

        // And the collection page itself is still reachable with no login.
        let response = reqwest::get(format!("http://{TEST_HOST}:{port}/collection/{slug}")).await?;
        assert!(response.status().is_success());
        Ok(())
    }

    /// A collection created through the web interface must be drillable:
    /// discovered, listed on the landing page, and reachable by slug.
    #[tokio::test]
    async fn test_a_locally_created_collection_can_be_drilled() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let data_dir = dir.path().to_path_buf();

        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: Vec::new(),
            data_dir: Some(data_dir.clone()),
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let client = reqwest::Client::new();
        let base = format!("http://{TEST_HOST}:{port}");

        client
            .post(format!("{base}/files/folder"))
            .form(&[("parent", ""), ("name", "Spanish")])
            .send()
            .await?;
        client
            .post(format!("{base}/files/file"))
            .form(&[("parent", "Spanish"), ("name", "verbs")])
            .send()
            .await?;

        // The landing page must offer it, or nothing can be drilled.
        let landing = client.get(&base).send().await?.text().await?;
        assert!(
            landing.contains("/collection/Spanish"),
            "landing page did not list the local collection, got: {landing}"
        );

        // And it must be reachable by slug.
        let page = client
            .get(format!("{base}/collection/Spanish"))
            .send()
            .await?;
        assert!(
            page.status().is_success(),
            "collection page returned {}",
            page.status()
        );
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
                owner: None,
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
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

    /// The collection list starts a session in one tap. It used to lead to a
    /// topic picker whose own button also said "Drill", so studying always
    /// cost two presses of the same word.
    #[tokio::test]
    async fn test_drill_from_the_list_lands_on_a_card() -> Fallible<()> {
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Alpha.md"), "Q: What is 1+1?\nA: 2\n")?;
        write(coll_dir.join("Beta.md"), "Q: What is 2+2?\nA: 4\n")?;
        let slug = "test-collection";
        let port = spawn_test_server(coll_dir, slug).await?;
        let base = format!("http://{TEST_HOST}:{port}");
        let client = reqwest::Client::new();

        // The list's Drill button carries no topic checkboxes at all.
        let list = client.get(format!("{base}/")).send().await?.text().await?;
        assert!(
            list.contains(r#"name="all_topics""#),
            "the list must post the one-tap start: {list}"
        );

        let body = client
            .post(format!("{base}/collection/{slug}/start"))
            .body("all_topics=1")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?
            .text()
            .await?;
        // Card one, not the topic picker.
        assert!(
            body.contains("value=\"Reveal\""),
            "one tap must land on a card: {body}"
        );
        assert!(
            !body.contains("deck-tree"),
            "the topic picker must not stand in the way: {body}"
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
        assert!(body.contains("Select at least one topic"), "body: {body}");
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
            .post(format!("http://{TEST_HOST}:{port}/sources/add"))
            .body("url=")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        let body = response.text().await?;
        assert!(
            body.contains("Enter a HedgeDoc note or git file URL"),
            "body: {body}"
        );
        assert!(body.contains("flash-error"));
        Ok(())
    }

    /// Regression test (BUG-38): an http:// HedgeDoc URL must be rejected with
    /// a flash message before anything is persisted, not stored with a
    /// permanent "Error" status.
    #[tokio::test]
    async fn test_hedgedoc_add_rejects_http_url_before_persisting() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let data_dir = dir.path().to_path_buf();
        let config_path = data_dir.join("hashcards.toml");
        write(
            &config_path,
            format!("[server]\ndata_dir = {:?}\n", data_dir.to_string_lossy()),
        )?;

        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![],
            data_dir: Some(data_dir.clone()),
            config_path: Some(config_path.clone()),
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        // reqwest follows the 303 redirect, so `response` is the /sources
        // manage page rendered with the flash query params.
        let response = reqwest::Client::new()
            .post(format!("http://{TEST_HOST}:{port}/sources/add"))
            .form(&[("url", "http://notes.example.com/abc123")])
            .send()
            .await?;
        assert!(response.status().is_success());
        let body = response.text().await?;
        assert!(
            body.contains("HTTPS"),
            "expected the HTTPS validation error on the manage page, got: {body}"
        );

        // Nothing may have been persisted to the config file.
        let config_content = read_to_string(&config_path)?;
        assert!(
            !config_content.contains("notes.example.com"),
            "rejected URL was persisted: {config_content}"
        );
        Ok(())
    }

    /// BUG-45 regression: after a session finishes, the landing page must
    /// show refreshed due counts without a manual sync or Home action.
    #[tokio::test]
    async fn test_landing_counts_refresh_after_session_finish() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Deck.md"), "Q: What is 1+1?\nA: 2\n")?;

        let slug = "count-collection".to_string();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Count Collection".to_string(),
                slug: slug.clone(),
                coll_dir: coll_dir.clone(),
                db_path: coll_dir.join("hashcards.db"),
                owner: None,
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let base = format!("http://{TEST_HOST}:{port}");
        let client = reqwest::Client::new();

        // Sanity: the one new card is due, so the landing page offers a drill.
        let body = client.get(format!("{base}/")).send().await?.text().await?;
        assert!(
            body.contains("Drill"),
            "expected a due card before the session: {body}"
        );

        // Start and complete the one-card session.
        client
            .post(format!("{base}/collection/{slug}/start"))
            .body("decks=Deck")
            .header("content-type", "application/x-www-form-urlencoded")
            .send()
            .await?;
        for action in ["Reveal", "Good"] {
            client
                .post(format!("{base}/collection/{slug}"))
                .body(format!("action={action}"))
                .header("content-type", "application/x-www-form-urlencoded")
                .send()
                .await?;
        }

        // The refresh runs in the background; poll the landing page briefly.
        let mut refreshed = false;
        for _ in 0..40 {
            let body = client.get(format!("{base}/")).send().await?.text().await?;
            if body.contains("Nothing due") {
                refreshed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        assert!(
            refreshed,
            "landing page still shows stale due counts after the session finished"
        );
        Ok(())
    }

    /// FEAT-03: an unfinished session is offered for resumption on the
    /// landing page and is NOT silently discarded by another start POST.
    #[tokio::test]
    async fn test_unfinished_session_is_offered_for_resume_not_replaced() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(
            coll_dir.join("Deck.md"),
            "Q: What is 1+1?\nA: 2\n\n---\n\nQ: What is 2+2?\nA: 4\n",
        )?;

        let slug = "resume-collection".to_string();
        let db_path = coll_dir.join("hashcards.db");
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Resume Collection".to_string(),
                slug: slug.clone(),
                coll_dir: coll_dir.clone(),
                db_path: db_path.clone(),
                owner: None,
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let base = format!("http://{TEST_HOST}:{port}");
        let client = reqwest::Client::new();
        let start = || {
            client
                .post(format!("{base}/collection/{slug}/start"))
                .body("decks=Deck")
                .header("content-type", "application/x-www-form-urlencoded")
                .send()
        };
        start().await?;

        // The landing page offers to resume the running two-card session.
        let body = client.get(format!("{base}/")).send().await?.text().await?;
        assert!(
            body.contains("Resume (2 left)"),
            "landing page must offer resume: {body}"
        );

        // A second start POST must not discard the session: still one DB row.
        start().await?;
        let db_path_str = db_path
            .to_str()
            .ok_or_else(|| crate::error::ErrorReport::new("non-UTF-8 temp path"))?;
        let db = Database::new(db_path_str)?;
        assert_eq!(
            db.get_all_sessions()?.len(),
            1,
            "second start POST must not create a new session"
        );
        Ok(())
    }

    /// FEAT-03: a dangling DB session row (left by a crash/restart) is closed
    /// and reported on the deck browser page. It cannot be rehydrated: the
    /// card queue only exists in memory.
    #[tokio::test]
    async fn test_dangling_session_row_is_closed_and_reported() -> Fallible<()> {
        let port = pick_unused_port().unwrap();
        let dir = tempdir()?;
        let coll_dir = dir.path().to_path_buf();
        write(coll_dir.join("Deck.md"), "Q: What is 1+1?\nA: 2\n")?;
        let db_path = coll_dir.join("hashcards.db");

        // Simulate a crash: a session row that was never closed.
        {
            let db_path_str = db_path
                .to_str()
                .ok_or_else(|| crate::error::ErrorReport::new("non-UTF-8 temp path"))?;
            let db = Database::new(db_path_str)?;
            let t0 = Timestamp::try_from("2026-01-01T10:00:00.000".to_string())?;
            db.create_session(t0)?;
        }

        let slug = "dangling-collection".to_string();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: vec![ResolvedCollection {
                name: "Dangling Collection".to_string(),
                slug: slug.clone(),
                coll_dir: coll_dir.clone(),
                db_path: db_path.clone(),
                owner: None,
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let body = reqwest::get(format!("http://{TEST_HOST}:{port}/collection/{slug}"))
            .await?
            .text()
            .await?;
        assert!(
            body.contains("interrupted session"),
            "deck browser must report the closed interrupted session: {body}"
        );

        // The sweep runs once, at startup, so the notice is reported once.
        // Re-closing on every browse would also stamp `ended_at` on sessions
        // that are still live in a concurrent CLI `drill`.
        let second = reqwest::get(format!("http://{TEST_HOST}:{port}/collection/{slug}"))
            .await?
            .text()
            .await?;
        assert!(
            !second.contains("interrupted session"),
            "the notice must not reappear on every visit: {second}"
        );
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
                owner: None,
            }],
            data_dir: None,
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
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

    /// A file name may hold `#` or `?`, which end a URL's path. The tree
    /// links to it and the editor posts back to it percent-encoded, and axum
    /// decodes the path parameter, so the round trip has to land on the file
    /// the user actually clicked.
    #[tokio::test]
    async fn test_a_file_named_with_a_url_delimiter_opens_in_the_editor() -> Fallible<()> {
        let dir = tempdir()?;
        let data_dir = dir.path().to_path_buf();
        let port = pick_unused_port().unwrap();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: Vec::new(),
            data_dir: Some(data_dir.clone()),
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let folder = data_dir.join("local").join("default").join("Spanish");
        std::fs::create_dir_all(&folder)?;
        write(folder.join("a#b.md"), "Q: the cat\nA: el gato\n")?;

        let tree = reqwest::get(format!("http://{TEST_HOST}:{port}/files"))
            .await?
            .text()
            .await?;
        assert!(
            tree.contains("/files/edit/Spanish/a%23b.md"),
            "tree: {tree}"
        );

        let page = reqwest::get(format!(
            "http://{TEST_HOST}:{port}/files/edit/Spanish/a%23b.md"
        ))
        .await?
        .text()
        .await?;
        assert!(page.contains("el gato"), "page: {page}");
        Ok(())
    }

    /// The whole paste path, end to end: the bytes go up, the markdown path
    /// comes back, a card that references it saves, the collection still
    /// loads, and the image itself is served back through the collection's
    /// file endpoint. Any one of those links breaking leaves the user with a
    /// card that shows a broken picture — or a collection that will not open.
    #[tokio::test]
    async fn test_a_pasted_image_is_stored_referenced_and_served() -> Fallible<()> {
        let dir = tempdir()?;
        let data_dir = dir.path().to_path_buf();
        let port = pick_unused_port().unwrap();
        let config = ResolvedServeConfig {
            host: TEST_HOST.to_string(),
            port,
            git: None,
            defaults: DefaultsSection::default(),
            collections: Vec::new(),
            data_dir: Some(data_dir.clone()),
            config_path: None,
            hedgedoc_entries: Vec::new(),
            custom_decks: Vec::new(),
            session_timeout_minutes: 1440,
            oidc: None,
        };
        spawn(async move { start_serve(config).await });
        wait_for_server(TEST_HOST, port).await?;

        let deck_dir = data_dir.join("local").join("default").join("Spanish");
        std::fs::create_dir_all(&deck_dir)?;
        write(deck_dir.join("verbs.md"), "Q: a\nA: b\n")?;

        // A one-pixel PNG, header and all: the endpoint reads the magic
        // number, so a fake body would be refused.
        let png: Vec<u8> = {
            let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
            v.extend_from_slice(b"the rest of a very small picture");
            v
        };
        let client = reqwest::Client::new();
        let base = format!("http://{TEST_HOST}:{port}");

        let response = client
            .post(format!("{base}/files/media/Spanish/verbs.md"))
            .header("content-type", "image/png")
            .body(png.clone())
            .send()
            .await?;
        assert_eq!(response.status(), 200);
        let inserted = response.text().await?;
        assert!(inserted.starts_with("@/media/"), "got: {inserted}");

        // Anything that is not an image is refused, and says why.
        let refused = client
            .post(format!("{base}/files/media/Spanish/verbs.md"))
            .body(b"%PDF-1.7\n".to_vec())
            .send()
            .await?;
        assert_eq!(refused.status(), 400);
        assert!(refused.text().await?.contains("PNG"), "no reason given");

        // A card referencing it saves, and the collection still loads: a
        // reference to a missing file fails `validate_media_files` and takes
        // the whole collection page down with it.
        let page = reqwest::get(format!("{base}/files/edit/Spanish/verbs.md"))
            .await?
            .text()
            .await?;
        let mtime = page
            .split("name=\"mtime\" value=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default()
            .to_string();
        let saved = client
            .post(format!("{base}/files/edit/Spanish/verbs.md"))
            .form(&[
                ("mtime", mtime.as_str()),
                ("content", &format!("Q: what is this\nA: ![]({inserted})\n")),
            ])
            .send()
            .await?;
        assert_eq!(saved.status(), 200, "the save redirect must land");

        let collection = reqwest::get(format!("{base}/collection/Spanish"))
            .await?
            .text()
            .await?;
        assert!(
            !collection.contains("Missing media"),
            "collection: {collection}"
        );

        // And the image comes back through the collection's own endpoint,
        // which is what the rendered card asks for.
        let name = inserted.trim_start_matches("@/");
        let served = reqwest::get(format!("{base}/collection/Spanish/file/{name}")).await?;
        assert_eq!(served.status(), 200);
        assert_eq!(
            served
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(served.bytes().await?.to_vec(), png);
        Ok(())
    }
}
