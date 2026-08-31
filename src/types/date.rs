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

use std::fmt::Display;
use std::fmt::Formatter;

use chrono::Days;
use chrono::Local;
use chrono::NaiveDate;
use rusqlite::ToSql;

use crate::error::Fallible;
use crate::error::fail;
use rusqlite::types::FromSql;
use rusqlite::types::FromSqlError;
use rusqlite::types::FromSqlResult;
use rusqlite::types::ToSqlOutput;
use rusqlite::types::ValueRef;
use serde::Serialize;

use crate::error::ErrorReport;

/// Represents a date.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Date(NaiveDate);

impl Date {
    pub fn new(naive_date: NaiveDate) -> Self {
        Self(naive_date)
    }

    pub fn today() -> Self {
        Self(Local::now().naive_local().date())
    }

    pub fn into_inner(self) -> NaiveDate {
        self.0
    }

    /// This date plus `n` days.
    pub fn add_days(self, n: u64) -> Fallible<Date> {
        match self.0.checked_add_days(Days::new(n)) {
            Some(d) => Ok(Date(d)),
            None => fail(format!("Date out of range: {self} plus {n} days.")),
        }
    }

    /// This date minus `n` days.
    pub fn sub_days(self, n: u64) -> Fallible<Date> {
        match self.0.checked_sub_days(Days::new(n)) {
            Some(d) => Ok(Date(d)),
            None => fail(format!("Date out of range: {self} minus {n} days.")),
        }
    }

    /// Signed number of days from `self` to `other` (positive if `other` is later).
    pub fn days_until(self, other: Date) -> i64 {
        other.0.signed_duration_since(self.0).num_days()
    }
}

impl Display for Date {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.format("%Y-%m-%d"))
    }
}

impl TryFrom<String> for Date {
    type Error = ErrorReport;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let date = NaiveDate::parse_from_str(&value, "%Y-%m-%d")
            .map_err(|_| ErrorReport::new(format!("invalid date: {}", value)))?;
        Ok(Date(date))
    }
}

impl ToSql for Date {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let str = self.to_string();
        Ok(ToSqlOutput::from(str))
    }
}

impl FromSql for Date {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let string: String = FromSql::column_result(value)?;
        Date::try_from(string).map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

impl Serialize for Date {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let s = self.to_string();
        serializer.serialize_str(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Fallible;

    #[test]
    fn test_serialize() -> Fallible<()> {
        let date = Date::new(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap());
        let serialized = serde_json::to_string(&date)?;
        assert_eq!(serialized, "\"2024-01-02\"");
        Ok(())
    }

    #[test]
    fn test_add_days() -> Fallible<()> {
        let d = Date::try_from("2026-08-31".to_string())?;
        assert_eq!(d.add_days(0)?, d);
        assert_eq!(d.add_days(1)?.to_string(), "2026-09-01");
        assert_eq!(d.add_days(30)?.to_string(), "2026-09-30");
        Ok(())
    }

    #[test]
    fn test_sub_days() -> Fallible<()> {
        let d = Date::try_from("2026-08-31".to_string())?;
        assert_eq!(d.sub_days(89)?.to_string(), "2026-06-03");
        Ok(())
    }

    #[test]
    fn test_days_until() -> Fallible<()> {
        let a = Date::try_from("2026-08-31".to_string())?;
        let b = Date::try_from("2026-09-05".to_string())?;
        assert_eq!(a.days_until(b), 5);
        assert_eq!(b.days_until(a), -5);
        assert_eq!(a.days_until(a), 0);
        Ok(())
    }
}
