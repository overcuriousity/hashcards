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

use crate::collection::Collection;
use crate::error::Fallible;
use crate::parser::DuplicateCard;

pub fn check_collection(directory: Option<String>) -> Fallible<()> {
    let collection = Collection::new(directory)?;
    for line in duplicate_report(&collection.duplicates) {
        println!("{line}");
    }
    println!("ok");
    Ok(())
}

/// One warning line per duplicate card, naming both file:line locations.
fn duplicate_report(duplicates: &[DuplicateCard]) -> Vec<String> {
    duplicates
        .iter()
        .map(|duplicate| format!("warning: {duplicate}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs::write;

    use super::check_collection;
    use crate::collection::Collection;
    use crate::error::Fallible;
    use crate::helper::create_tmp_copy_of_test_directory;
    use crate::helper::create_tmp_directory;

    use super::duplicate_report;

    #[test]
    fn test_non_existent_directory() {
        assert!(check_collection(Some("./derpherp".to_string())).is_err());
    }

    #[test]
    fn test_directory() -> Fallible<()> {
        let directory = create_tmp_copy_of_test_directory()?;
        assert!(check_collection(Some(directory)).is_ok());
        Ok(())
    }

    #[test]
    fn test_check_reports_duplicates_and_succeeds() -> Fallible<()> {
        let dir = create_tmp_directory()?;
        write(dir.join("a.md"), "Q: same question\nA: same answer\n")?;
        write(dir.join("b.md"), "Q: same question\nA: same answer\n")?;
        let dir_string = dir.display().to_string();

        // `check` succeeds: duplicates are a warning, not an error.
        assert!(check_collection(Some(dir_string.clone())).is_ok());

        // The report lists the duplicate with both file:line locations.
        let collection = Collection::new(Some(dir_string))?;
        let report = duplicate_report(&collection.duplicates);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("a.md:1"), "report was: {}", report[0]);
        assert!(report[0].contains("b.md:1"), "report was: {}", report[0]);
        Ok(())
    }

    #[test]
    fn test_duplicate_report_empty_without_duplicates() {
        assert!(duplicate_report(&[]).is_empty());
    }
}
