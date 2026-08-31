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

use chrono::Duration;
use chrono::NaiveDate;

use crate::fsrs::Difficulty;
use crate::fsrs::Grade;
use crate::fsrs::Interval;
use crate::fsrs::Recall;
use crate::fsrs::Stability;
use crate::fsrs::initial_difficulty;
use crate::fsrs::initial_stability;
use crate::fsrs::interval;
use crate::fsrs::new_difficulty;
use crate::fsrs::new_stability;
use crate::fsrs::retrievability;
use crate::types::date::Date;
use crate::types::timestamp::Timestamp;

/// The desired recall probability.
const TARGET_RECALL: f64 = 0.9;

/// The minimum review interval in days.
const MIN_INTERVAL: f64 = 1.0;

/// The maximum review interval in days.
const MAX_INTERVAL: f64 = 256.0;

/// Represents performance information for a card.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Performance {
    /// The card is new, and has never been reviewed.
    New,
    /// The card has been reviewed at least once.
    Reviewed(ReviewedPerformance),
}

impl Performance {
    pub fn is_new(&self) -> bool {
        matches!(self, Performance::New)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReviewedPerformance {
    /// The timestamp when the card was last reviewed.
    pub last_reviewed_at: Timestamp,
    /// The card's stability (an FSRS parameter).
    pub stability: Stability,
    /// The card's difficulty (an FSRS parameter).
    pub difficulty: Difficulty,
    /// The FSRS-calculated interval in days until the next review. This is
    /// the raw interval, before any rounding and clamping.
    pub interval_raw: Interval,
    /// The FSRS interval as an integer number of days.
    pub interval_days: i64,
    /// The card's next due date.
    pub due_date: Date,
    /// The number of times the card has been reviewed.
    pub review_count: usize,
}

pub fn update_performance(
    perf: Performance,
    grade: Grade,
    reviewed_at: Timestamp,
) -> ReviewedPerformance {
    let today: NaiveDate = reviewed_at.date().into_inner();
    let (stability, difficulty, review_count): (Stability, Difficulty, usize) = match perf {
        Performance::New => (initial_stability(grade), initial_difficulty(grade), 0),
        Performance::Reviewed(ReviewedPerformance {
            last_reviewed_at,
            stability,
            difficulty,
            review_count,
            ..
        }) => {
            let last_reviewed_at: NaiveDate = last_reviewed_at.date().into_inner();
            // Clamp: a clock rollback can put `last_reviewed_at` in the
            // future; negative elapsed time makes retrievability exceed 1.0
            // or go NaN (BUG-28).
            let time: Interval = ((today - last_reviewed_at).num_days() as f64).max(0.0);
            let retr: Recall = retrievability(time, stability);
            let stability: Stability = new_stability(difficulty, stability, retr, grade);
            let difficulty: Difficulty = new_difficulty(difficulty, grade);
            (stability, difficulty, review_count)
        }
    };
    let interval_raw: Interval = interval(TARGET_RECALL, stability);
    let interval_rounded: Interval = interval_raw.round();
    let interval_clamped: Interval = interval_rounded.clamp(MIN_INTERVAL, MAX_INTERVAL);
    let interval_days: i64 = interval_clamped as i64;
    let interval_duration: Duration = Duration::days(interval_days);
    let due_date: Date = Date::new(today + interval_duration);
    ReviewedPerformance {
        last_reviewed_at: reviewed_at,
        stability,
        difficulty,
        interval_raw,
        interval_days,
        due_date,
        review_count: review_count + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-2
    }

    #[test]
    fn test_new() {
        assert!(Performance::New.is_new());
        let reviewed_at = Timestamp::now();
        let reviewed_perf = update_performance(Performance::New, Grade::Good, reviewed_at);
        assert!(!Performance::Reviewed(reviewed_perf).is_new());
    }

    #[test]
    fn test_update_new_card() {
        let reviewed_at = Timestamp::now();
        let result = update_performance(Performance::New, Grade::Good, reviewed_at);
        let ReviewedPerformance {
            last_reviewed_at,
            stability,
            difficulty,
            interval_raw,
            interval_days,
            due_date: _,
            review_count,
        } = result;
        assert_eq!(last_reviewed_at, reviewed_at);
        assert!(approx_eq(stability, 3.17));
        assert!(approx_eq(difficulty, 5.28));
        assert!(approx_eq(interval_raw, 3.17));
        assert_eq!(interval_days, 3);
        assert_eq!(review_count, 1);
    }

    #[test]
    fn test_update_already_reviewed_card() {
        let now = Timestamp::now();
        let today = now.date();
        let duration = Duration::days(3);
        let last_reviewed_at = Timestamp::new(now.into_inner() - duration);
        let initial_perf = ReviewedPerformance {
            last_reviewed_at,
            stability: 3.17,
            difficulty: 5.28,
            interval_raw: 3.17,
            interval_days: 3,
            due_date: Date::new(today.into_inner() + duration),
            review_count: 1,
        };
        let reviewed_at = now;
        let result = update_performance(
            Performance::Reviewed(initial_perf),
            Grade::Easy,
            reviewed_at,
        );
        let ReviewedPerformance {
            last_reviewed_at,
            stability,
            difficulty,
            interval_raw,
            interval_days,
            due_date: _,
            review_count,
        } = result;
        assert_eq!(last_reviewed_at, reviewed_at);
        assert!(approx_eq(stability, 25.80));
        assert!(approx_eq(difficulty, 4.50));
        assert!(approx_eq(interval_raw, 25.80));
        assert_eq!(interval_days, 26);
        assert_eq!(review_count, 2);
    }

    /// Regression test for BUG-28: a `last_reviewed_at` in the future (clock
    /// rollback) must not produce negative elapsed time. Elapsed days clamp
    /// to zero, so retrievability is exactly 1.0 and the update stays finite.
    #[test]
    fn test_future_last_reviewed_at_is_clamped() {
        let now = Timestamp::now();
        let today = now.date();

        // Case A: one day in the future. Without clamping, retrievability
        // exceeds 1.0 and stability shrinks on a Good grade.
        let one_day = Duration::days(1);
        let perf = ReviewedPerformance {
            last_reviewed_at: Timestamp::new(now.into_inner() + one_day),
            stability: 3.17,
            difficulty: 5.28,
            interval_raw: 3.17,
            interval_days: 3,
            due_date: Date::new(today.into_inner() + one_day),
            review_count: 1,
        };
        let result = update_performance(Performance::Reviewed(perf), Grade::Good, now);
        assert!(result.stability.is_finite());
        assert!(result.interval_raw.is_finite());
        // With elapsed time clamped to 0, retrievability is 1.0 and a Good
        // grade leaves stability unchanged.
        assert!(approx_eq(result.stability, 3.17));
        assert_eq!(result.interval_days, 3);

        // Case B: five days in the future with low stability. Without
        // clamping, retrievability's base goes negative and powf() is NaN.
        let five_days = Duration::days(5);
        let perf = ReviewedPerformance {
            last_reviewed_at: Timestamp::new(now.into_inner() + five_days),
            stability: 1.0,
            difficulty: 5.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: Date::new(today.into_inner() + five_days),
            review_count: 1,
        };
        let result = update_performance(Performance::Reviewed(perf), Grade::Good, now);
        assert!(result.stability.is_finite());
        assert!(result.interval_raw.is_finite());
        assert!(result.interval_days >= 1);
    }
}
