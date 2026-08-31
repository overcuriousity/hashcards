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

use std::error::Error;
use std::fmt::Display;
use std::fmt::Formatter;
use std::path::StripPrefixError;
use std::string::FromUtf8Error;

use crate::parser::ParserError;

#[derive(Debug, PartialEq)]
pub struct ErrorReport {
    message: String,
}

impl ErrorReport {
    pub fn new(msg: impl Into<String>) -> Self {
        ErrorReport {
            message: msg.into(),
        }
    }

    /// The message without the "error: " display prefix.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<std::io::Error> for ErrorReport {
    fn from(value: std::io::Error) -> Self {
        ErrorReport {
            message: format!("I/O error: {value}"),
        }
    }
}

impl From<StripPrefixError> for ErrorReport {
    fn from(value: StripPrefixError) -> Self {
        ErrorReport {
            message: format!("Path prefix error: {value}"),
        }
    }
}

impl From<walkdir::Error> for ErrorReport {
    fn from(value: walkdir::Error) -> Self {
        ErrorReport {
            message: format!("Directory traversal error: {value}"),
        }
    }
}

impl From<rusqlite::Error> for ErrorReport {
    fn from(value: rusqlite::Error) -> Self {
        ErrorReport {
            message: format!("Database error: {value}"),
        }
    }
}

impl From<reqwest::Error> for ErrorReport {
    fn from(value: reqwest::Error) -> Self {
        ErrorReport {
            message: format!("HTTP error: {value}"),
        }
    }
}

impl From<toml::ser::Error> for ErrorReport {
    fn from(value: toml::ser::Error) -> Self {
        ErrorReport {
            message: format!("TOML serialization error: {value}"),
        }
    }
}

impl From<FromUtf8Error> for ErrorReport {
    fn from(value: FromUtf8Error) -> Self {
        ErrorReport {
            message: format!("UTF-8 conversion error: {value}"),
        }
    }
}

impl From<serde_json::Error> for ErrorReport {
    fn from(value: serde_json::Error) -> Self {
        ErrorReport {
            message: format!("JSON error: {value}"),
        }
    }
}

impl From<ParserError> for ErrorReport {
    fn from(value: ParserError) -> Self {
        ErrorReport {
            message: value.to_string(),
        }
    }
}

impl From<toml::de::Error> for ErrorReport {
    fn from(value: toml::de::Error) -> Self {
        ErrorReport {
            message: format!("TOML error: {value}"),
        }
    }
}

impl Display for ErrorReport {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "error: {}", self.message)
    }
}

impl Error for ErrorReport {
    fn description(&self) -> &str {
        &self.message
    }
}

pub type Fallible<T> = Result<T, ErrorReport>;

pub fn fail<T>(msg: impl Into<String>) -> Fallible<T> {
    Err(ErrorReport {
        message: msg.into(),
    })
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::path::PathBuf;

    use super::*;

    /// BUG-21: I/O errors must render as human-readable messages, not
    /// multi-line `{:#?}` debug spew.
    #[test]
    fn test_io_error_is_human_readable() {
        let io_err = std::io::Error::new(ErrorKind::NotFound, "file missing");
        let report = ErrorReport::from(io_err);
        assert_eq!(report.to_string(), "error: I/O error: file missing");
    }

    /// BUG-21: converting a ParserError must not stack a redundant
    /// "Parse error:" prefix onto the "error:" prefix.
    #[test]
    fn test_parser_error_has_no_double_prefix() {
        let parser_err = ParserError {
            message: "Cloze deletion is empty.".to_string(),
            file_path: PathBuf::from("deck.md"),
            line_num: 4,
        };
        let report = ErrorReport::from(parser_err);
        assert_eq!(
            report.to_string(),
            "error: Cloze deletion is empty. Location: deck.md:5"
        );
    }
}
