use maud::Markup;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::config::slugify;
use crate::cmd::serve::files::TreeEntry;
use crate::cmd::serve::files::parse_buffer;
use crate::cmd::serve::local::LocalRoot;
use crate::error::Fallible;
use crate::flash::Flash;
use crate::markdown::MarkdownRenderConfig;
use crate::media::resolve::MediaResolverBuilder;

/// The file manager: the whole tree, with per-row rename and delete, and a
/// create form for each folder.
pub fn render_tree_page(tree: &[TreeEntry], flash: Option<Flash>) -> Markup {
    page_template(html! {
        div.landing {
            @if let Some(f) = &flash { (f.render()) }
            h1 { "My Cards" }
            p { a.back-link href="/" { "← Back to collections" } }

            p.hint {
                "Each top-level folder is a collection. Files inside it are decks."
            }

            form.inline-form action="/files/folder" method="post" {
                input type="hidden" name="parent" value="";
                input type="text" name="name" placeholder="New collection name" required;
                input type="submit" value="Add collection";
            }

            @if tree.is_empty() {
                p.notice { "No cards yet. Create a collection to start." }
            }

            ul.file-tree {
                @for entry in tree {
                    li style=(format!("margin-left: {}rem", entry.depth)) {
                        @if entry.is_dir {
                            span.folder { (entry.name) }
                            form.inline-form action="/files/file" method="post" {
                                input type="hidden" name="parent" value=(entry.rel_path);
                                input type="text" name="name" placeholder="new-file.md" required;
                                input type="submit" value="Add file";
                            }
                            form.inline-form action="/files/folder" method="post" {
                                input type="hidden" name="parent" value=(entry.rel_path);
                                input type="text" name="name" placeholder="subfolder" required;
                                input type="submit" value="Add folder";
                            }
                        } @else {
                            a href=(format!("/files/edit/{}", entry.rel_path)) { (entry.name) }
                        }
                        form.inline-form action="/files/rename" method="post" {
                            input type="hidden" name="path" value=(entry.rel_path);
                            input type="text" name="name" placeholder="rename to" required;
                            input type="submit" value="Rename";
                        }
                        form.inline-form action="/files/delete" method="post" {
                            input type="hidden" name="path" value=(entry.rel_path);
                            input type="submit" value="Delete";
                        }
                    }
                }
            }
        }
    })
}

/// The document editor: raw markdown on the left with insert buttons, a
/// live parse preview on the right.
pub fn render_editor_page(
    rel_path: &str,
    content: &str,
    mtime: u64,
    flash: Option<Flash>,
) -> Markup {
    page_template(html! {
        div.editor {
            @if let Some(f) = &flash { (f.render()) }
            h1 { (rel_path) }
            p { a.back-link href="/files" { "← Back to my cards" } }

            div.editor-toolbar {
                button type="button" data-snippet="Q: \nA: " { "Q/A" }
                button type="button" data-snippet="C: The [answer] goes here." { "Cloze" }
                button type="button" data-snippet="T: \nD: " { "Term" }
                button type="button" data-snippet="\n---\n" { "Separator" }
                button type="button" data-snippet="$$\n\n$$" { "LaTeX" }
                button type="button" data-snippet="![](image.png)" { "Image" }
            }

            div.editor-panes {
                form #editor-form action=(format!("/files/edit/{rel_path}")) method="post" {
                    input type="hidden" name="mtime" value=(mtime);
                    textarea #editor-text name="content" spellcheck="false" { (content) }
                    input type="submit" value="Save";
                }
                div #preview .preview-pane {
                    p.hint { "Start typing to see your cards." }
                }
            }
        }
    })
}

/// Render an unsaved buffer's parse result.
///
/// Returns a fragment, not a page: the editor swaps it into the preview
/// pane. Errors carry their line number so the user can find them in the
/// textarea.
pub fn render_preview(root: &LocalRoot, rel_path: &str, content: &str) -> Markup {
    let parsed = match parse_buffer(rel_path, content) {
        Ok(p) => p,
        Err(e) => {
            return html! {
                div.preview-error {
                    p { "This file does not parse yet." }
                    p.error-detail { (e.to_string()) }
                }
            };
        }
    };

    let config = match preview_render_config(root, rel_path) {
        Ok(c) => c,
        Err(e) => {
            return html! { div.preview-error { p { (e.to_string()) } } };
        }
    };

    html! {
        p.preview-count { (format!("{} cards", parsed.cards.len())) }
        @if !parsed.duplicates.is_empty() {
            p.preview-warning {
                (format!("{} duplicate cards will be skipped.", parsed.duplicates.len()))
            }
        }
        @for card in &parsed.cards {
            div.preview-card {
                div.preview-front {
                    @match card.html_front(&config) {
                        Ok(m) => (m),
                        Err(e) => p.error-detail { (e.to_string()) },
                    }
                }
                div.preview-back {
                    @match card.html_back(&config) {
                        Ok(m) => (m),
                        Err(e) => p.error-detail { (e.to_string()) },
                    }
                }
            }
        }
    }
}

/// Resolve media the way the collection itself will: relative to the
/// top-level folder, which is the collection root. A file sitting loose in
/// the user's root has no collection, so the root stands in for one.
fn preview_render_config(root: &LocalRoot, rel_path: &str) -> Fallible<MarkdownRenderConfig> {
    let trimmed = rel_path.trim_matches('/');
    let (coll_dir, deck_rel, slug) = match trimmed.split_once('/') {
        Some((top, rest)) => (root.path().join(top), rest.to_string(), slugify(top)),
        None => (
            root.path().to_path_buf(),
            trimmed.to_string(),
            String::new(),
        ),
    };
    Ok(MarkdownRenderConfig {
        resolver: MediaResolverBuilder::new()
            .with_collection_path(coll_dir)?
            .with_deck_path(std::path::PathBuf::from(deck_rel))?
            .build()?,
        file_url_prefix: format!("/collection/{slug}/file"),
    })
}
