use maud::Markup;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::config::slugify;
use crate::cmd::serve::files::TreeEntry;
use crate::cmd::serve::files::parse_buffer;
use crate::cmd::serve::href::encoded_path;
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
                            a href=(format!("/files/edit/{}", encoded_path(&entry.rel_path))) { (entry.name) }
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
                // The preview fetch needs the path as it is on disk, which
                // the encoded action cannot be turned back into safely, so
                // it travels beside it.
                form #editor-form data-path=(rel_path)
                    action=(format!("/files/edit/{}", encoded_path(rel_path))) method="post" {
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
                    p.error-detail { (e.message()) }
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
///
/// The path comes straight from the browser, so it is resolved inside the
/// user's root like every other client-supplied path, rather than joined
/// onto it.
fn preview_render_config(root: &LocalRoot, rel_path: &str) -> Fallible<MarkdownRenderConfig> {
    let trimmed = rel_path.trim_matches('/');
    let (coll_dir, deck_rel, slug) = match trimmed.split_once('/') {
        Some((top, rest)) => (root.resolve(top)?, rest.to_string(), slugify(top)),
        None => (root.resolve(".")?, trimmed.to_string(), String::new()),
    };
    Ok(MarkdownRenderConfig {
        resolver: MediaResolverBuilder::new()
            .with_collection_path(coll_dir)?
            .with_deck_path(std::path::PathBuf::from(deck_rel))?
            .build()?,
        file_url_prefix: format!("/collection/{slug}/file"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::serve::files::TreeEntry;
    use crate::cmd::serve::local::LocalRoot;
    use crate::error::Fallible;
    use crate::helper::create_tmp_directory;

    /// The preview is the one place a client-supplied path reaches the
    /// filesystem without going through the file manager, so it has to be
    /// resolved inside the user's root like every other path.
    #[test]
    fn preview_refuses_a_path_outside_the_root() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        let root = LocalRoot::for_user(&dir, None)?;
        assert!(preview_render_config(&root, "../../etc/x.md").is_err());
        assert!(preview_render_config(&root, "/etc/x.md").is_err());
        Ok(())
    }

    /// `validate_name` allows `#` and `?`, which end a URL's path. Left raw
    /// in an href, the browser would drop everything after them and the
    /// editor would open the wrong file — or none.
    #[test]
    fn tree_links_encode_reserved_characters() {
        let tree = vec![TreeEntry {
            rel_path: "Spanish/a#b?c.md".to_string(),
            name: "a#b?c.md".to_string(),
            is_dir: false,
            depth: 1,
        }];
        let html = render_tree_page(&tree, None).into_string();
        assert!(
            html.contains("/files/edit/Spanish/a%23b%3Fc.md"),
            "got: {html}"
        );
    }

    #[test]
    fn the_editor_form_posts_to_the_encoded_path() {
        let html = render_editor_page("Spanish/a#b.md", "", 0, None).into_string();
        assert!(html.contains("/files/edit/Spanish/a%23b.md"), "got: {html}");
        // The preview fetch needs the raw path, not the URL, so it travels
        // in its own attribute rather than being parsed back out.
        assert!(
            html.contains(r#"data-path="Spanish/a#b.md""#),
            "got: {html}"
        );
    }
}
