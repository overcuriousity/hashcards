use maud::Markup;
use maud::html;

use crate::cmd::drill::template::page_template;
use crate::cmd::serve::files::TreeEntry;
use crate::flash::Flash;

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
