use maud::Markup;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::href::safe_href;
use crate::cmd::serve::state::HedgedocSource;
use crate::flash::Flash;
use crate::types::timestamp::Timestamp;

pub fn render_manage_page(
    sources: &[HedgedocSource],
    last_synced: Option<Timestamp>,
    config_available: bool,
    flash: Option<Flash>,
) -> Markup {
    page_template(html! {
        @if let Some(f) = &flash { (f.render()) }
        div.landing {
            h1 { "HedgeDoc Sources" }
            p { a href="/" { "← Back to collections" } }

            @if !config_available {
                div.notice {
                    p { "HedgeDoc sources cannot be managed without a configured data directory." }
                    p { "To enable HedgeDoc source management, start hashcards with " code { "--config hashcards.toml" } "." }
                }
            } @else {
                div.sync-bar {
                    span.sync-status {
                        @if let Some(ts) = last_synced {
                            (format!("Last synced: {}", ts.into_inner().format("%Y-%m-%d %H:%M:%S")))
                        } @else {
                            "Not yet synced"
                        }
                    }
                    form.inline-form action="/hedgedoc/sync" method="post" {
                        input .sync-button type="submit" value="Sync All";
                    }
                }

                h2 { "Add Source" }
                form.add-source-form action="/hedgedoc/add" method="post" {
                    div.add-source-row {
                        input
                            .add-source-url
                            type="url"
                            name="url"
                            placeholder="https://notes.example.com/noteId"
                            required;
                        input type="submit" value="Add" .sync-button;
                    }
                }

                @if sources.is_empty() {
                    p.empty { "No HedgeDoc sources configured." }
                } @else {
                    h2 { "Sources" }
                    table.collection-table {
                        thead {
                            tr {
                                th { "Source" }
                                th { "Deck" }
                                th { "URL" }
                                th { "Status" }
                                th { "" }
                            }
                        }
                        tbody {
                            @for src in sources {
                                @let note = &src.note;
                                tr {
                                    td { (src.source_uri) }
                                    td { (note.deck_name) }
                                    td.source-url-cell {
                                        @if let Some(href) = safe_href(&note.url) {
                                            a href=(href) target="_blank" rel="noopener noreferrer" { (note.url) }
                                        } @else {
                                            span { (note.url) }
                                        }
                                    }
                                    td {
                                        @if let Some(ref err) = note.last_error {
                                            span.status-error title=(err) { "Error" }
                                        } @else {
                                            span.status-ok { "OK" }
                                        }
                                    }
                                    td {
                                        form action="/hedgedoc/delete" method="post" {
                                            input type="hidden" name="url" value=(note.url);
                                            input type="submit" value="Delete" .sync-button
                                                onclick="return confirm('Remove this HedgeDoc note?')";
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cmd::serve::config::ResolvedCollection;
    use crate::cmd::serve::state::HedgedocNote;

    fn source_with_url(url: &str) -> HedgedocSource {
        HedgedocSource {
            source_uri: "https://notes.example.com".to_string(),
            collection: ResolvedCollection {
                name: "Notes".to_string(),
                slug: "hedgedoc-notes".to_string(),
                coll_dir: PathBuf::from("/tmp/notes"),
                db_path: PathBuf::from("/tmp/notes.db"),
                owner: None,
            },
            note: HedgedocNote {
                url: url.to_string(),
                deck_name: "deck".to_string(),
                file_name: "deck.md".to_string(),
                last_error: None,
            },
        }
    }

    #[test]
    fn manage_page_never_links_unsafe_url_schemes() {
        // Regression test for BUG-24: even if a bad URL reaches the state
        // (hand-edited config, pre-fix persisted data), it must not become
        // a clickable link.
        let sources = vec![source_with_url("javascript:alert(1)")];
        let html = render_manage_page(&sources, None, true, None).into_string();
        assert!(
            !html.contains(r#"href="javascript:"#),
            "unsafe scheme must not be rendered as a link: {html}"
        );
        assert!(
            html.contains("javascript:alert(1)"),
            "the URL should still be visible as plain text: {html}"
        );
    }

    #[test]
    fn manage_page_links_https_urls() {
        let sources = vec![source_with_url("https://notes.example.com/abc")];
        let html = render_manage_page(&sources, None, true, None).into_string();
        assert!(
            html.contains(r#"href="https://notes.example.com/abc""#),
            "https URLs must still be linked: {html}"
        );
    }
}
