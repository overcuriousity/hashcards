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

/// Escape a string for interpolation into a single-quoted JavaScript
/// literal in the generated KaTeX macro script.
pub fn escape_js_string_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('`', "\\`")
        .replace('$', "\\$")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_answer_controls_display() {
        assert_eq!(AnswerControls::Full.to_string(), "full");
        assert_eq!(AnswerControls::Binary.to_string(), "binary");
    }

    #[test]
    fn test_escape_js_string_literal() {
        assert_eq!(escape_js_string_literal(r"\alpha"), r"\\alpha");
        assert_eq!(escape_js_string_literal("a`b"), "a\\`b");
        assert_eq!(escape_js_string_literal("$x$"), "\\$x\\$");
    }
}
