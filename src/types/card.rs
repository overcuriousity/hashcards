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

use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;

use maud::Markup;
use maud::PreEscaped;
use maud::html;

use crate::error::Fallible;
use crate::error::fail;
use crate::markdown::MarkdownRenderConfig;
use crate::markdown::markdown_to_html;
use crate::markdown::markdown_to_html_inline;
use crate::types::aliases::DeckName;
use crate::types::card_hash::CardHash;
use crate::types::card_hash::Hasher;

/// Return a marker string that is guaranteed not to occur in `text`.
///
/// Used to stand in for a cloze deletion while the card's markdown is
/// rendered to HTML; the containment check makes it impossible for card
/// text to forge the marker (BUG-22). The marker is plain ASCII letters,
/// digits, and hyphens, so pulldown-cmark passes it through unchanged.
fn cloze_marker(text: &str) -> String {
    let mut n: u64 = 0;
    loop {
        let marker = format!("HASHCARDS-CLOZE-{n}");
        if !text.contains(&marker) {
            return marker;
        }
        n += 1;
    }
}

#[derive(Clone)]
pub struct Card {
    /// The name of the deck this card belongs to.
    deck_name: DeckName,
    /// The absolute path of the file this card was parsed from.
    file_path: PathBuf,
    /// The line number range that contains the card.
    range: (usize, usize),
    /// The card's content.
    content: CardContent,
    /// The cached hash of the card's content.
    hash: CardHash,
}

#[derive(Clone)]
pub enum CardContent {
    Basic {
        question: String,
        answer: String,
    },
    Cloze {
        /// The text of the card without brackets.
        text: String,
        /// The byte position (not character position) of the first byte of
        /// the deletion within `text`.
        start: usize,
        /// The byte position (not character position) of the last byte of
        /// the deletion within `text`, inclusive.
        end: usize,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum CardType {
    Basic,
    Cloze,
}

impl Card {
    pub fn new(
        deck_name: DeckName,
        file_path: PathBuf,
        range: (usize, usize),
        content: CardContent,
    ) -> Self {
        let hash = content.hash();
        Self {
            deck_name,
            file_path,
            content,
            range,
            hash,
        }
    }

    pub fn deck_name(&self) -> &DeckName {
        &self.deck_name
    }

    pub fn content(&self) -> &CardContent {
        &self.content
    }

    pub fn hash(&self) -> CardHash {
        self.hash
    }

    pub fn family_hash(&self) -> Option<CardHash> {
        self.content.family_hash()
    }

    /// Return the absolute path of the file this card was parsed from.
    pub fn file_path(&self) -> &PathBuf {
        &self.file_path
    }

    /// Return the path of the file this card was parsed from, relative to the
    /// collection root directory.
    ///
    /// e.g., if the collection root is `/foo/bar/` and the file path is
    /// `/foo/bar/baz/deck.md`, this returns `baz/deck.md`.
    pub fn relative_file_path(&self, collection_root: &Path) -> Fallible<PathBuf> {
        let canon_root: PathBuf = collection_root.canonicalize()?;
        let canon_file: PathBuf = self.file_path.canonicalize()?;
        let result: PathBuf = canon_file.strip_prefix(&canon_root)?.to_path_buf();
        Ok(result)
    }

    pub fn range(&self) -> (usize, usize) {
        self.range
    }

    pub fn card_type(&self) -> CardType {
        match &self.content {
            CardContent::Basic { .. } => CardType::Basic,
            CardContent::Cloze { .. } => CardType::Cloze,
        }
    }

    /// A short plain-text preview of the card's question (or cloze prompt),
    /// trimmed and truncated to 120 bytes at a char boundary, for display in
    /// lists and stats.
    pub fn preview(&self) -> String {
        let raw = match &self.content {
            CardContent::Basic { question, .. } => question.as_str(),
            CardContent::Cloze { text, .. } => text.as_str(),
        };
        let trimmed = raw.trim();
        if trimmed.len() > 120 {
            // Truncate at a char boundary before 120 bytes.
            let mut end = 120;
            while !trimmed.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…", &trimmed[..end])
        } else {
            trimmed.to_string()
        }
    }

    pub fn html_front(&self, config: &MarkdownRenderConfig) -> Fallible<Markup> {
        self.content.html_front(config)
    }

    pub fn html_back(&self, config: &MarkdownRenderConfig) -> Fallible<Markup> {
        self.content.html_back(config)
    }
}

impl CardContent {
    pub fn new_basic(question: impl Into<String>, answer: impl Into<String>) -> Self {
        Self::Basic {
            question: question.into().trim().to_string(),
            answer: answer.into().trim().to_string(),
        }
    }

    pub fn new_cloze(prompt: impl Into<String>, start: usize, end: usize) -> Self {
        Self::Cloze {
            text: prompt.into(),
            start,
            end,
        }
    }

    pub fn hash(&self) -> CardHash {
        let mut hasher = Hasher::new();
        match &self {
            CardContent::Basic { question, answer } => {
                hasher.update(b"Basic");
                hasher.update(question.as_bytes());
                hasher.update(answer.as_bytes());
            }
            CardContent::Cloze { text, start, end } => {
                let bytes = text.as_bytes();
                // Positions are byte positions (see CLAUDE.md). A malformed
                // range hashes as an empty deletion instead of panicking.
                let deletion: &[u8] = bytes.get(*start..=*end).unwrap_or(&[]);
                let occurrence = occurrence_index(bytes, *start, deletion);
                // The hash input is fully platform-independent: 0xFF never
                // occurs in UTF-8, so it is an unambiguous field separator,
                // and the occurrence index is serialized as decimal ASCII —
                // no fixed-width integers, no endianness, no pointer width.
                hasher.update(b"ClozeV2");
                hasher.update(bytes);
                hasher.update(&[0xFF]);
                hasher.update(deletion);
                hasher.update(&[0xFF]);
                hasher.update(occurrence.to_string().as_bytes());
            }
        }
        hasher.finalize()
    }

    /// All cloze cards derived from the same text have the same family hash.
    ///
    /// For basic cards, this is `None`.
    pub fn family_hash(&self) -> Option<CardHash> {
        match &self {
            CardContent::Basic { .. } => None,
            CardContent::Cloze { text, .. } => {
                let mut hasher = Hasher::new();
                hasher.update(b"Cloze");
                hasher.update(text.as_bytes());
                Some(hasher.finalize())
            }
        }
    }

    pub fn html_front(&self, config: &MarkdownRenderConfig) -> Fallible<Markup> {
        let html = match self {
            CardContent::Basic { question, .. } => {
                html! {
                    (PreEscaped(markdown_to_html(config, question)?))
                }
            }
            CardContent::Cloze { text, start, end } => {
                let range = deletion_range(text, *start, *end)?;
                let marker = cloze_marker(text);
                let mut text_bytes: Vec<u8> = text.as_bytes().to_owned();
                text_bytes.splice(range, marker.bytes());
                let text: String = String::from_utf8(text_bytes)?;
                let text: String = markdown_to_html(config, &text)?;
                let text: String =
                    text.replace(&marker, "<span class='cloze'>.............</span>");
                html! {
                    (PreEscaped(text))
                }
            }
        };
        Ok(html)
    }

    pub fn html_back(&self, config: &MarkdownRenderConfig) -> Fallible<Markup> {
        let html = match self {
            CardContent::Basic { answer, .. } => {
                html! {
                    (PreEscaped(markdown_to_html(config, answer)?))
                }
            }
            CardContent::Cloze { text, start, end } => {
                let range = deletion_range(text, *start, *end)?;
                let marker = cloze_marker(text);
                let mut text_bytes: Vec<u8> = text.as_bytes().to_owned();
                let deleted_text: Vec<u8> = text_bytes[range.clone()].to_owned();
                let deleted_text: String = String::from_utf8(deleted_text)?;
                let deleted_text: String = markdown_to_html_inline(config, &deleted_text)?;
                text_bytes.splice(range, marker.bytes());
                let text: String = String::from_utf8(text_bytes)?;
                let text = markdown_to_html(config, &text)?;
                let text = text.replace(
                    &marker,
                    &format!("<span class='cloze-reveal'>{}</span>", deleted_text),
                );
                html! {
                    (PreEscaped(text))
                }
            }
        };
        Ok(html)
    }
}

/// Validate a cloze deletion's inclusive byte range against its text.
///
/// Positions are byte positions (see CLAUDE.md), so this is a byte-wise
/// check. `hash()` cannot fail and degrades to an empty deletion, but the
/// render paths return `Fallible` and must report a malformed range rather
/// than index out of bounds.
fn deletion_range(text: &str, start: usize, end: usize) -> Fallible<Range<usize>> {
    if start > end || end >= text.len() {
        return fail(format!(
            "This cloze card has a deletion covering bytes {start}..={end}, \
             which does not fit its {} bytes of text. The card is corrupt; \
             re-saving the deck it came from will rebuild it.",
            text.len()
        ));
    }
    Ok(start..end + 1)
}

/// The number of occurrences of `deletion` that begin before byte position
/// `start` in `bytes`. Sibling deletions in a card are disjoint, so an
/// earlier identical deletion always counts as an occurrence — this is what
/// keeps two identical deletions in the same card hashing differently.
/// Byte-wise on purpose: cloze positions are byte positions.
fn occurrence_index(bytes: &[u8], start: usize, deletion: &[u8]) -> usize {
    if deletion.is_empty() {
        return 0;
    }
    (0..start.min(bytes.len()))
        .filter(|&p| bytes[p..].starts_with(deletion))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_card_hash() {
        let card1 = CardContent::new_basic("What is 2+2?", "4");
        let card2 = CardContent::new_basic("What is 2+2?", "4");
        let card3 = CardContent::new_basic("What is 3+3?", "6");
        assert_eq!(card1.hash(), card2.hash());
        assert_ne!(card1.hash(), card3.hash());
    }

    #[test]
    fn test_cloze_card_hash() {
        let a = CardContent::new_cloze("The capital of France is Paris", 0, 1);
        let b = CardContent::new_cloze("The capital of France is Paris", 0, 2);
        assert_eq!(a.family_hash(), b.family_hash());
    }

    /// A malformed cloze range must not panic. `hash()` already degrades to
    /// an empty deletion; the render paths return `Fallible`, so they must
    /// report the problem rather than index out of bounds.
    #[test]
    fn test_malformed_cloze_range_does_not_panic() -> Fallible<()> {
        let coll_path = crate::helper::create_tmp_directory()?;
        std::fs::write(coll_path.join("deck.md"), "")?;
        let config = MarkdownRenderConfig {
            resolver: crate::media::resolve::MediaResolverBuilder::new()
                .with_collection_path(coll_path)?
                .with_deck_path(std::path::PathBuf::from("deck.md"))?
                .build()?,
            file_url_prefix: "/file".to_string(),
        };
        // `end` is past the end of a 5-byte text.
        let content = CardContent::new_cloze("hello", 0, 99);
        let _ = content.hash();
        assert!(content.html_front(&config).is_err());
        assert!(content.html_back(&config).is_err());

        // `start` past `end` is malformed too.
        let inverted = CardContent::new_cloze("hello", 4, 1);
        let _ = inverted.hash();
        assert!(inverted.html_front(&config).is_err());
        assert!(inverted.html_back(&config).is_err());
        Ok(())
    }

    #[test]
    fn test_family_hash() {
        let a = CardContent::new_cloze("The capital of France is Paris", 0, 1);
        let b = CardContent::new_cloze("The capital of France is Paris", 0, 2);
        assert_eq!(a.family_hash(), b.family_hash());
    }

    use std::fs::write;

    use crate::helper::create_tmp_directory;
    use crate::media::resolve::MediaResolverBuilder;

    fn make_render_config() -> Fallible<MarkdownRenderConfig> {
        let coll_path: PathBuf = create_tmp_directory()?;
        let deck_path: PathBuf = coll_path.join("deck.md");
        write(&deck_path, "")?;
        Ok(MarkdownRenderConfig {
            resolver: MediaResolverBuilder::new()
                .with_collection_path(coll_path)?
                .with_deck_path(PathBuf::from("deck.md"))?
                .build()?,
            file_url_prefix: "http://localhost:1234/file".to_string(),
        })
    }

    /// BUG-26: rendering a cloze card with non-ASCII text before and inside
    /// the deletion round-trips through the byte-position splice without
    /// mangling multi-byte characters.
    #[test]
    fn test_non_ascii_cloze_render_round_trip() -> Fallible<()> {
        let config = make_render_config()?;
        // "Größe: " = 9 bytes; deletion "10 µm" = bytes 9..=14.
        let content = CardContent::new_cloze("Größe: 10 µm", 9, 14);

        let front = content.html_front(&config)?.into_string();
        assert!(front.contains("Größe:"), "front was: {front}");
        assert!(front.contains("<span class='cloze'>"), "front was: {front}");
        assert!(
            !front.contains("10 µm"),
            "deletion leaked into front: {front}"
        );

        let back = content.html_back(&config)?.into_string();
        assert!(
            back.contains("<span class='cloze-reveal'>10 µm</span>"),
            "back was: {back}"
        );
        assert!(back.contains("Größe:"), "back was: {back}");
        Ok(())
    }

    #[test]
    fn test_literal_cloze_sentinel_in_card_text_survives_rendering() -> Fallible<()> {
        // Regression test for BUG-22: a card containing the literal string
        // `CLOZE_DELETION` must render that text verbatim, with exactly one
        // cloze span (for the actual deletion).
        let text = "The string CLOZE_DELETION marks Paris in the code";
        let start = text.find("Paris").unwrap();
        let end = start + "Paris".len() - 1;
        let content = CardContent::new_cloze(text, start, end);
        let config = make_render_config()?;

        let front = content.html_front(&config)?.into_string();
        assert!(
            front.contains("CLOZE_DELETION"),
            "literal sentinel text must survive front rendering: {front}"
        );
        assert_eq!(
            front.matches("<span class='cloze'>").count(),
            1,
            "exactly one cloze blank expected: {front}"
        );

        let back = content.html_back(&config)?.into_string();
        assert!(
            back.contains("CLOZE_DELETION"),
            "literal sentinel text must survive back rendering: {back}"
        );
        assert!(
            back.contains("<span class='cloze-reveal'>Paris</span>"),
            "deletion must be revealed on the back: {back}"
        );
        assert_eq!(
            back.matches("<span class='cloze-reveal'>").count(),
            1,
            "exactly one reveal span expected: {back}"
        );
        Ok(())
    }

    #[test]
    fn test_preview_basic_short() {
        let card = Card::new(
            "Deck".to_string(),
            PathBuf::from("/tmp/deck.md"),
            (1, 2),
            CardContent::new_basic("  What is 2+2?  ", "4"),
        );
        assert_eq!(card.preview(), "What is 2+2?");
    }

    #[test]
    fn test_preview_truncates_at_char_boundary() {
        // 40 four-byte emoji = 160 bytes; the cut at 120 lands on a boundary.
        let long: String = "🦀".repeat(40);
        let card = Card::new(
            "Deck".to_string(),
            PathBuf::from("/tmp/deck.md"),
            (1, 2),
            CardContent::new_basic(long.clone(), "answer"),
        );
        let preview = card.preview();
        assert!(preview.ends_with('…'));
        assert!(preview.len() <= 124); // 120 bytes + 3-byte ellipsis, boundary-safe
        assert!(preview.starts_with("🦀"));
    }

    #[test]
    fn test_preview_cloze_uses_text() {
        let card = Card::new(
            "Deck".to_string(),
            PathBuf::from("/tmp/deck.md"),
            (1, 2),
            CardContent::new_cloze("Paris is the capital of France", 0, 4),
        );
        assert_eq!(card.preview(), "Paris is the capital of France");
    }

    /// BUG-27 regression: the cloze hash is a platform-independent function
    /// of (text, deletion content, occurrence index) — verified against the
    /// reference formula, which contains no offsets, no usize serialization,
    /// no endianness.
    #[test]
    fn test_cloze_hash_is_content_based() {
        let content = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        let mut hasher = Hasher::new();
        hasher.update(b"ClozeV2");
        hasher.update(b"The capital of France is Paris");
        hasher.update(&[0xFF]);
        hasher.update(b"Paris");
        hasher.update(&[0xFF]);
        hasher.update(b"0");
        assert_eq!(content.hash(), hasher.finalize());
    }

    /// Same property for multi-byte (non-ASCII) deletions: the deletion is
    /// the byte slice text[start..=end].
    #[test]
    fn test_cloze_hash_is_content_based_non_ascii() {
        let content = CardContent::new_cloze("je bois du café", 11, 15);
        let mut hasher = Hasher::new();
        hasher.update(b"ClozeV2");
        hasher.update("je bois du café".as_bytes());
        hasher.update(&[0xFF]);
        hasher.update("café".as_bytes());
        hasher.update(&[0xFF]);
        hasher.update(b"0");
        assert_eq!(content.hash(), hasher.finalize());
    }

    /// BUG-27 regression: the cloze hash must not mix in byte offsets, which
    /// were platform-dependent (`usize::to_le_bytes`).
    #[test]
    fn test_cloze_hash_no_longer_uses_offsets() {
        let content = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        let mut offset_based: Vec<u8> = Vec::new();
        offset_based.extend_from_slice(b"Cloze");
        offset_based.extend_from_slice(b"The capital of France is Paris");
        offset_based.extend_from_slice(&25usize.to_le_bytes());
        offset_based.extend_from_slice(&29usize.to_le_bytes());
        assert_ne!(content.hash(), CardHash::hash_bytes(&offset_based));
    }

    /// The basic-card hash is unchanged: only the cloze scheme moved.
    #[test]
    fn test_basic_card_hash_is_stable() {
        let content = CardContent::new_basic("Q", "A");
        // And the basic hash itself is unchanged from the old scheme.
        let mut hasher = Hasher::new();
        hasher.update(b"Basic");
        hasher.update(b"Q");
        hasher.update(b"A");
        assert_eq!(content.hash(), hasher.finalize());
    }

    /// Two identical deletions in the same card ("C: [a] and [a]") must
    /// still hash differently — the occurrence index disambiguates them.
    #[test]
    fn test_repeated_identical_deletions_hash_differently() {
        // Clean text "a and a": deletions at bytes (0,0) and (6,6).
        let first = CardContent::new_cloze("a and a", 0, 0);
        let second = CardContent::new_cloze("a and a", 6, 6);
        assert_ne!(first.hash(), second.hash());
        assert_eq!(first.family_hash(), second.family_hash());
    }

    /// Two different deletions in the same card hash differently.
    #[test]
    fn test_distinct_deletions_hash_differently() {
        let a = CardContent::new_cloze("The capital of France is Paris", 15, 20);
        let b = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        assert_ne!(a.hash(), b.hash());
    }

    /// Hand-written duplicate cards keep colliding, exactly as today.
    #[test]
    fn test_duplicate_cards_still_collide() {
        let a = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        let b = CardContent::new_cloze("The capital of France is Paris", 25, 29);
        assert_eq!(a.hash(), b.hash());
    }
}
