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

use std::collections::HashSet;

use rusqlite::Connection;
use rusqlite::Transaction;
use rusqlite::config::DbConfig;
use rusqlite::params;

use crate::error::Fallible;
use crate::error::fail;
use crate::fsrs::Difficulty;
use crate::fsrs::Grade;
use crate::fsrs::Stability;
use crate::types::card_hash::CardHash;
use crate::types::date::Date;
use crate::types::performance::Performance;
use crate::types::performance::ReviewedPerformance;
use crate::types::timestamp::Timestamp;

pub struct Database {
    conn: Connection,
}

pub struct ReviewRecord {
    pub card_hash: CardHash,
    pub reviewed_at: Timestamp,
    pub grade: Grade,
    pub stability: f64,
    pub difficulty: f64,
    pub interval_raw: f64,
    pub interval_days: i64,
    pub due_date: Date,
    pub duration_ms: Option<i64>,
}

pub struct SessionRow {
    pub session_id: i64,
    pub started_at: Timestamp,
    pub ended_at: Timestamp,
}

pub struct ReviewRow {
    pub review_id: i64,
    pub data: ReviewRecord,
}

pub struct Bookmark {
    pub card_hash: CardHash,
    pub note: Option<String>,
    pub created_at: Timestamp,
}

impl Database {
    pub fn new(database_path: &str) -> Fallible<Self> {
        let mut conn = Connection::open(database_path)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)?;
        {
            let tx = conn.transaction()?;
            if !probe_schema_exists(&tx)? {
                tx.execute_batch(include_str!("schema.sql"))?;
            } else {
                migrate_add_duration_ms(&tx)?;
                migrate_add_bookmarks(&tx)?;
                migrate_add_voided(&tx)?;
            }
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// Insert a new card in the database.
    ///
    /// If a card with the given hash exists, returns an error.
    pub fn insert_card(&self, card_hash: CardHash, added_at: Timestamp) -> Fallible<()> {
        if self.card_exists(card_hash)? {
            return fail("Card already exists");
        }
        let sql = "insert into cards (card_hash, added_at, review_count) values (?, ?, 0);";
        self.conn.execute(sql, params![card_hash, added_at])?;
        Ok(())
    }

    /// Return the set of all card hashes in the database.
    pub fn card_hashes(&self) -> Fallible<HashSet<CardHash>> {
        let sql = "select card_hash from cards;";
        let mut stmt = self.conn.prepare(sql)?;
        let card_iter = stmt.query_map([], |row| {
            let card_hash: CardHash = row.get(0)?;
            Ok(card_hash)
        })?;
        let mut card_hashes = HashSet::new();
        for card in card_iter {
            card_hashes.insert(card?);
        }
        Ok(card_hashes)
    }

    /// Find the hashes of the cards due today.
    pub fn due_today(&self, today: Date) -> Fallible<HashSet<CardHash>> {
        let mut due = HashSet::new();
        let sql = "select card_hash, due_date from cards;";
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query(params![])?;
        while let Some(row) = rows.next()? {
            let hash: CardHash = row.get(0)?;
            let due_date: Option<Date> = row.get(1)?;
            match due_date {
                None => {
                    // Never reviewed, so it's due.
                    due.insert(hash);
                }
                Some(due_date) => {
                    if due_date <= today {
                        due.insert(hash);
                    }
                }
            }
        }
        Ok(due)
    }

    /// Get a card's performance information.
    pub fn get_card_performance_opt(&self, card_hash: CardHash) -> Fallible<Option<Performance>> {
        let sql = "select last_reviewed_at, stability, difficulty, interval_raw, interval_days, due_date, review_count from cards where card_hash = ?;";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![card_hash], |row| {
            let last_reviewed_at: Option<Timestamp> = row.get(0)?;
            let stability: Option<Stability> = row.get(1)?;
            let difficulty: Option<Difficulty> = row.get(2)?;
            let interval_raw: Option<f64> = row.get(3)?;
            let interval_days: Option<i64> = row.get(4)?;
            let due_date: Option<Date> = row.get(5)?;
            let review_count: i32 = row.get(6)?;
            if let (
                Some(last_reviewed_at),
                Some(stability),
                Some(difficulty),
                Some(interval_raw),
                Some(interval_days),
                Some(due_date),
            ) = (
                last_reviewed_at,
                stability,
                difficulty,
                interval_raw,
                interval_days,
                due_date,
            ) {
                Ok(Performance::Reviewed(ReviewedPerformance {
                    last_reviewed_at,
                    stability,
                    difficulty,
                    interval_raw,
                    interval_days,
                    due_date,
                    review_count: review_count as usize,
                }))
            } else {
                Ok(Performance::New)
            }
        })?;
        if let Some(row) = rows.into_iter().next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
    }

    /// Get a card's performance information. If the card does not exist,
    /// returns an error.
    pub fn get_card_performance(&self, card_hash: CardHash) -> Fallible<Performance> {
        match self.get_card_performance_opt(card_hash)? {
            Some(performance) => Ok(performance),
            None => fail(format!(
                "No performance data found for card with hash {card_hash}"
            )),
        }
    }

    /// Update a card's performance information.
    ///
    /// If no card with the given hash exists, returns an error.
    ///
    /// Test-only: production grading updates performance inside the same
    /// transaction as the review, via
    /// [`Database::insert_review_and_update_performance`].
    #[cfg(test)]
    pub fn update_card_performance(
        &self,
        card_hash: CardHash,
        performance: Performance,
    ) -> Fallible<()> {
        if !self.card_exists(card_hash)? {
            return fail("Card not found");
        }
        let (
            last_reviewed_at,
            stability,
            difficulty,
            interval_raw,
            interval_days,
            due_date,
            review_count,
        ) = match performance {
            Performance::New => (None, None, None, None, None, None, 0),
            Performance::Reviewed(rp) => (
                Some(rp.last_reviewed_at),
                Some(rp.stability),
                Some(rp.difficulty),
                Some(rp.interval_raw),
                Some(rp.interval_days as i32),
                Some(rp.due_date),
                rp.review_count as i32,
            ),
        };
        let sql = "update cards set last_reviewed_at = ?, stability = ?, difficulty = ?, interval_raw = ?, interval_days = ?, due_date = ?, review_count = ? where card_hash = ?;";
        let params = params![
            last_reviewed_at,
            stability,
            difficulty,
            interval_raw,
            interval_days,
            due_date,
            review_count,
            card_hash
        ];
        self.conn.execute(sql, params)?;
        Ok(())
    }

    /// Create a new session row at session start. Returns the session_id.
    /// ended_at is initially set to started_at as a placeholder; call
    /// close_session when the session finishes.
    pub fn create_session(&self, started_at: Timestamp) -> Fallible<i64> {
        let sql = "insert into sessions (started_at, ended_at) values (?, ?) returning session_id;";
        let session_id: i64 =
            self.conn
                .query_row(sql, params![started_at, started_at], |row| row.get(0))?;
        Ok(session_id)
    }

    /// Atomically insert a review and update the card's performance.
    ///
    /// Both operations run inside a single transaction so a crash between them
    /// cannot leave the DB in an inconsistent state. Returns the new review_id.
    pub fn insert_review_and_update_performance(
        &mut self,
        session_id: i64,
        review: &ReviewRecord,
        performance: Performance,
    ) -> Fallible<i64> {
        let tx = self.conn.transaction()?;
        let review_id: i64 = {
            let sql = "insert into reviews (session_id, card_hash, reviewed_at, grade, stability, difficulty, interval_raw, interval_days, due_date, duration_ms) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) returning review_id;";
            tx.query_row(
                sql,
                params![
                    session_id,
                    review.card_hash,
                    review.reviewed_at,
                    review.grade,
                    review.stability,
                    review.difficulty,
                    review.interval_raw,
                    review.interval_days as i32,
                    review.due_date,
                    review.duration_ms
                ],
                |row| row.get(0),
            )?
        };
        update_card_performance_tx(&tx, review.card_hash, performance)?;
        tx.commit()?;
        Ok(review_id)
    }

    /// Atomically void a review and restore prior card performance (undo).
    ///
    /// Both operations run inside a single transaction so a crash between them
    /// cannot leave the DB in an inconsistent state.
    pub fn void_review_and_restore_performance(
        &mut self,
        review_id: i64,
        card_hash: CardHash,
        prev_performance: Performance,
    ) -> Fallible<()> {
        let tx = self.conn.transaction()?;
        let rows = tx.execute("update reviews set voided = 1 where review_id = ? and card_hash = ?;", params![review_id, card_hash])?;
        if rows != 1 {
            return fail("review not found or does not belong to this card");
        }
        update_card_performance_tx(&tx, card_hash, prev_performance)?;
        tx.commit()?;
        Ok(())
    }

    /// Insert a single review without touching card performance.
    ///
    /// Test-only: production grading goes through
    /// [`Database::insert_review_and_update_performance`], which keeps the
    /// review and the card's performance in one transaction.
    #[cfg(test)]
    pub fn insert_review_immediately(
        &self,
        session_id: i64,
        review: &ReviewRecord,
    ) -> Fallible<i64> {
        let sql = "insert into reviews (session_id, card_hash, reviewed_at, grade, stability, difficulty, interval_raw, interval_days, due_date, duration_ms) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) returning review_id;";
        let review_id: i64 = self.conn.query_row(
            sql,
            params![
                session_id,
                review.card_hash,
                review.reviewed_at,
                review.grade,
                review.stability,
                review.difficulty,
                review.interval_raw,
                review.interval_days as i32,
                review.due_date,
                review.duration_ms
            ],
            |row| row.get(0),
        )?;
        Ok(review_id)
    }

    /// Update ended_at to mark a session as complete.
    pub fn close_session(&self, session_id: i64, ended_at: Timestamp) -> Fallible<()> {
        let sql = "update sessions set ended_at = ? where session_id = ?;";
        let rows = self.conn.execute(sql, params![ended_at, session_id])?;
        if rows != 1 {
            return fail(format!("No session with ID {session_id} to close"));
        }
        Ok(())
    }

    /// Delete a card. The foreign-key cascade removes its reviews and
    /// bookmarks in the same statement, so the deletion is atomic.
    ///
    /// Note that this permanently erases the card's review history; it does
    /// not go through the `voided` audit-trail model that undo uses.
    ///
    /// If no card with the given hash exists, returns an error.
    pub fn delete_card(&self, card_hash: CardHash) -> Fallible<()> {
        let sql = "delete from cards where card_hash = ?;";
        let rows = self.conn.execute(sql, params![card_hash])?;
        if rows != 1 {
            return fail("Card not found");
        }
        Ok(())
    }

    /// Does a card with the given hash exist?
    pub fn card_exists(&self, card_hash: CardHash) -> Fallible<bool> {
        let sql = "select count(*) from cards where card_hash = ?;";
        let count: i64 = self.conn.query_row(sql, [card_hash], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Insert a card only if it doesn't already exist.
    pub fn insert_card_if_new(&self, card_hash: CardHash, added_at: Timestamp) -> Fallible<()> {
        if !self.card_exists(card_hash)? {
            self.insert_card(card_hash, added_at)?;
        }
        Ok(())
    }

    /// Rename a card hash in-place. Updates `reviews` and `bookmarks` via ON UPDATE CASCADE.
    pub fn rename_card_hash(&self, old_hash: CardHash, new_hash: CardHash) -> Fallible<()> {
        if !self.card_exists(old_hash)? {
            return fail("Original card not found");
        }
        if self.card_exists(new_hash)? {
            return fail("A card with the new content already exists");
        }
        let sql = "update cards set card_hash = ? where card_hash = ?;";
        self.conn.execute(sql, params![new_hash, old_hash])?;
        Ok(())
    }

    /// Does a bookmark for this card hash exist?
    pub fn bookmark_exists(&self, card_hash: CardHash) -> Fallible<bool> {
        let sql = "select count(*) from bookmarks where card_hash = ?;";
        let count: i64 = self.conn.query_row(sql, [card_hash], |row| row.get(0))?;
        Ok(count > 0)
    }

    /// Insert or replace a bookmark. If the card has no DB row yet, it is created first.
    ///
    /// Unlike reviews, bookmarks write to the DB mid-session so they survive aborted sessions.
    pub fn insert_bookmark(
        &self,
        card_hash: CardHash,
        note: Option<String>,
        now: Timestamp,
    ) -> Fallible<()> {
        let sql = "insert or replace into bookmarks (card_hash, note, created_at) values (?, ?, ?);";
        self.conn.execute(sql, params![card_hash, note, now])?;
        Ok(())
    }

    /// Delete a bookmark.
    pub fn delete_bookmark(&self, card_hash: CardHash) -> Fallible<()> {
        let sql = "delete from bookmarks where card_hash = ?;";
        self.conn.execute(sql, params![card_hash])?;
        Ok(())
    }

    /// Get the bookmark for a card, if any.
    #[allow(dead_code)]
    pub fn get_bookmark(&self, card_hash: CardHash) -> Fallible<Option<Bookmark>> {
        let sql = "select note, created_at from bookmarks where card_hash = ?;";
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = stmt.query_map(params![card_hash], |row| {
            Ok(Bookmark {
                card_hash,
                note: row.get(0)?,
                created_at: row.get(1)?,
            })
        })?;
        if let Some(row) = rows.next() {
            Ok(Some(row?))
        } else {
            Ok(None)
        }
    }

    /// List all bookmarks, newest first.
    pub fn list_bookmarks(&self) -> Fallible<Vec<Bookmark>> {
        let sql = "select card_hash, note, created_at from bookmarks order by created_at desc;";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |row| {
            Ok(Bookmark {
                card_hash: row.get(0)?,
                note: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        let mut bookmarks = Vec::new();
        for row in rows {
            bookmarks.push(row?);
        }
        Ok(bookmarks)
    }

    /// Update the note on an existing bookmark.
    pub fn update_bookmark_note(
        &self,
        card_hash: CardHash,
        note: Option<String>,
    ) -> Fallible<()> {
        let sql = "update bookmarks set note = ? where card_hash = ?;";
        self.conn.execute(sql, params![note, card_hash])?;
        Ok(())
    }

    /// Count the number of bookmarks in the database.
    pub fn count_bookmarks(&self) -> Fallible<usize> {
        let sql = "select count(*) from bookmarks;";
        let count: i64 = self.conn.query_row(sql, [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Count the number of non-voided reviews performed on the given date.
    pub fn count_reviews_in_date(&self, date: Date) -> Fallible<usize> {
        let sql = "select count(*) from reviews where substr(reviewed_at, 1, 10) = ? and voided = 0;";
        let count: i64 = self.conn.query_row(sql, params![date], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Get the list of all sessions in the database.
    pub fn get_all_sessions(&self) -> Fallible<Vec<SessionRow>> {
        let sql = "select session_id, started_at, ended_at from sessions order by started_at;";
        let mut stmt = self.conn.prepare(sql)?;
        let session_iter = stmt.query_map([], |row| {
            Ok(SessionRow {
                session_id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
            })
        })?;
        let mut sessions = Vec::new();
        for session in session_iter {
            sessions.push(session?);
        }
        Ok(sessions)
    }

    /// Get the list of all non-voided reviews for a given session.
    pub fn get_reviews_for_session(&self, session_id: i64) -> Fallible<Vec<ReviewRow>> {
        let sql = "select review_id, card_hash, reviewed_at, grade, stability, difficulty, interval_raw, interval_days, due_date, duration_ms from reviews where session_id = ? and voided = 0 order by reviewed_at;";
        let mut stmt = self.conn.prepare(sql)?;
        let review_iter = stmt.query_map(params![session_id], |row| {
            Ok(ReviewRow {
                review_id: row.get(0)?,
                data: ReviewRecord {
                    card_hash: row.get(1)?,
                    reviewed_at: row.get(2)?,
                    grade: row.get(3)?,
                    stability: row.get(4)?,
                    difficulty: row.get(5)?,
                    interval_raw: row.get(6)?,
                    interval_days: row.get(7)?,
                    due_date: row.get(8)?,
                    duration_ms: row.get(9)?,
                },
            })
        })?;
        let mut reviews = Vec::new();
        for review in review_iter {
            reviews.push(review?);
        }
        Ok(reviews)
    }
}

fn update_card_performance_tx(tx: &Transaction, card_hash: CardHash, performance: Performance) -> Fallible<()> {
    let (
        last_reviewed_at,
        stability,
        difficulty,
        interval_raw,
        interval_days,
        due_date,
        review_count,
    ) = match performance {
        Performance::New => (None, None, None, None, None, None, 0i32),
        Performance::Reviewed(rp) => (
            Some(rp.last_reviewed_at),
            Some(rp.stability),
            Some(rp.difficulty),
            Some(rp.interval_raw),
            Some(rp.interval_days as i32),
            Some(rp.due_date),
            rp.review_count as i32,
        ),
    };
    let sql = "update cards set last_reviewed_at = ?, stability = ?, difficulty = ?, interval_raw = ?, interval_days = ?, due_date = ?, review_count = ? where card_hash = ?;";
    tx.execute(
        sql,
        params![
            last_reviewed_at,
            stability,
            difficulty,
            interval_raw,
            interval_days,
            due_date,
            review_count,
            card_hash
        ],
    )?;
    Ok(())
}

fn migrate_add_duration_ms(tx: &Transaction) -> Fallible<()> {
    let sql = "select count(*) from pragma_table_info('reviews') where name = 'duration_ms';";
    let count: i64 = tx.query_row(sql, [], |row| row.get(0))?;
    if count == 0 {
        tx.execute_batch("alter table reviews add column duration_ms integer;")?;
    }
    Ok(())
}

fn migrate_add_bookmarks(tx: &Transaction) -> Fallible<()> {
    let sql = "select count(*) from sqlite_master where type='table' AND name='bookmarks';";
    let count: i64 = tx.query_row(sql, [], |row| row.get(0))?;
    if count == 0 {
        tx.execute_batch(
            "create table bookmarks (
                card_hash text primary key
                    references cards (card_hash)
                    on update cascade
                    on delete cascade,
                note text,
                created_at text not null
            ) strict;",
        )?;
    }
    Ok(())
}

fn migrate_add_voided(tx: &Transaction) -> Fallible<()> {
    let sql = "select count(*) from pragma_table_info('reviews') where name = 'voided';";
    let count: i64 = tx.query_row(sql, [], |row| row.get(0))?;
    if count == 0 {
        tx.execute_batch("alter table reviews add column voided integer not null default 0;")?;
    }
    Ok(())
}

fn probe_schema_exists(tx: &Transaction) -> Fallible<bool> {
    let sql = "select count(*) from sqlite_master where type='table' AND name=?;";
    let count: i64 = tx.query_row(sql, ["cards"], |row| row.get(0))?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsrs::Grade;
    use crate::types::performance::ReviewedPerformance;

    #[test]
    fn test_probe_schema_exists() -> Fallible<()> {
        let mut conn = Connection::open_in_memory()?;
        let tx = conn.transaction()?;
        assert!(!probe_schema_exists(&tx)?);
        Ok(())
    }

    /// Insert a card, and see that its hash is returned by `card_hashes`, and
    /// that `get_card_performance` returns an initial empty performance, and
    /// `due_today` returns it since it's new.
    #[test]
    fn test_insert_card() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        let hashes = db.card_hashes()?;
        assert!(hashes.contains(&card_hash));
        let performance = db.get_card_performance(card_hash)?;
        assert_eq!(performance, Performance::New);
        let due_today = db.due_today(now.date())?;
        assert!(due_today.contains(&card_hash));
        Ok(())
    }

    /// Inserting a card twice returns an error.
    #[test]
    fn test_insert_twice() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        let result = db.insert_card(card_hash, now);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.to_string(), "error: Card already exists");
        Ok(())
    }

    /// Updating a card's performance, and checking that `get_card_performance`
    /// works and that `due_today` returns the card.
    #[test]
    fn test_update_performance() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        let performance = Performance::Reviewed(ReviewedPerformance {
            last_reviewed_at: now,
            stability: 2.0,
            difficulty: 2.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: now.date(),
            review_count: 1,
        });
        db.update_card_performance(card_hash, performance)?;
        let fetched_performance = db.get_card_performance(card_hash)?;
        assert_eq!(fetched_performance, performance);
        let due_today = db.due_today(now.date())?;
        assert!(due_today.contains(&card_hash));
        Ok(())
    }

    /// `get_card_performance` fails if the card does not exist.
    #[test]
    fn test_get_performance_nonexistent() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let result = db.get_card_performance(card_hash);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            format!("error: No performance data found for card with hash {card_hash}")
        );
        Ok(())
    }

    /// `update_card_performance` fails if the card does not exist.
    #[test]
    fn test_update_performance_nonexistent() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let performance = Performance::New;
        let result = db.update_card_performance(card_hash, performance);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.to_string(), "error: Card not found");
        Ok(())
    }

    /// Create a session, insert a review immediately, and close the session.
    #[test]
    fn test_session_persistence() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;

        let session_id = db.create_session(now)?;
        let review = ReviewRecord {
            card_hash,
            reviewed_at: now,
            grade: Grade::Good,
            stability: 2.0,
            difficulty: 2.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: now.date(),
            duration_ms: Some(3500),
        };
        let review_id = db.insert_review_immediately(session_id, &review)?;
        db.close_session(session_id, now)?;

        let sessions = db.get_all_sessions()?;
        assert_eq!(sessions.len(), 1);
        let session = &sessions[0];
        assert_eq!(session.started_at, now);
        assert_eq!(session.ended_at, now);
        let reviews = db.get_reviews_for_session(session.session_id)?;
        assert_eq!(reviews.len(), 1);
        let fetched_review = &reviews[0];
        assert_eq!(fetched_review.review_id, review_id);
        assert_eq!(fetched_review.data.card_hash, card_hash);
        assert_eq!(fetched_review.data.reviewed_at, now);
        assert_eq!(fetched_review.data.grade, Grade::Good);
        assert_eq!(fetched_review.data.stability, 2.0);
        assert_eq!(fetched_review.data.difficulty, 2.0);
        assert_eq!(fetched_review.data.interval_raw, 1.0);
        assert_eq!(fetched_review.data.interval_days, 1);
        assert_eq!(fetched_review.data.due_date, now.date());
        assert_eq!(fetched_review.data.duration_ms, Some(3500));
        Ok(())
    }

    /// Build a review record for `card_hash` with the given stability.
    #[cfg(test)]
    fn sample_review(card_hash: CardHash, now: Timestamp, stability: f64) -> ReviewRecord {
        ReviewRecord {
            card_hash,
            reviewed_at: now,
            grade: Grade::Good,
            stability,
            difficulty: 2.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: now.date(),
            duration_ms: None,
        }
    }

    /// Build a reviewed performance with the given stability and review count.
    #[cfg(test)]
    fn sample_performance(now: Timestamp, stability: f64, review_count: usize) -> Performance {
        Performance::Reviewed(ReviewedPerformance {
            last_reviewed_at: now,
            stability,
            difficulty: 2.0,
            interval_raw: 1.0,
            interval_days: 1,
            due_date: now.date(),
            review_count,
        })
    }

    /// Grading writes the review and the card's performance in one transaction.
    #[test]
    fn test_insert_review_and_update_performance() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        assert!(matches!(db.get_card_performance(card_hash)?, Performance::New));

        let session_id = db.create_session(now)?;
        let review = sample_review(card_hash, now, 2.0);
        let performance = sample_performance(now, 2.0, 1);
        let review_id =
            db.insert_review_and_update_performance(session_id, &review, performance)?;

        // The review landed...
        let reviews = db.get_reviews_for_session(session_id)?;
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].review_id, review_id);

        // ...and the card's performance moved with it.
        match db.get_card_performance(card_hash)? {
            Performance::Reviewed(rp) => {
                assert_eq!(rp.stability, 2.0);
                assert_eq!(rp.review_count, 1);
            }
            Performance::New => panic!("expected card performance to be updated"),
        }
        Ok(())
    }

    /// Undo voids the review and restores the card's prior performance, atomically.
    #[test]
    fn test_void_review_and_restore_performance() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;

        let session_id = db.create_session(now)?;
        // First grade: stability 2.0, one review.
        let first = sample_review(card_hash, now, 2.0);
        db.insert_review_and_update_performance(session_id, &first, sample_performance(now, 2.0, 1))?;
        // Second grade: stability 5.0, two reviews.
        let second = sample_review(card_hash, now, 5.0);
        let second_id = db.insert_review_and_update_performance(
            session_id,
            &second,
            sample_performance(now, 5.0, 2),
        )?;
        assert_eq!(db.get_reviews_for_session(session_id)?.len(), 2);

        // Undo the second grade, restoring the performance the card had before it.
        db.void_review_and_restore_performance(second_id, card_hash, sample_performance(now, 2.0, 1))?;

        // The voided review is excluded from read paths...
        let reviews = db.get_reviews_for_session(session_id)?;
        assert_eq!(reviews.len(), 1);
        assert_ne!(reviews[0].review_id, second_id);
        assert_eq!(db.count_reviews_in_date(now.date())?, 1);

        // ...and the card's performance rolled back.
        match db.get_card_performance(card_hash)? {
            Performance::Reviewed(rp) => {
                assert_eq!(rp.stability, 2.0);
                assert_eq!(rp.review_count, 1);
            }
            Performance::New => panic!("expected card to still be reviewed"),
        }
        Ok(())
    }

    /// Voiding a review that does not belong to the given card is rejected,
    /// and leaves the card's performance untouched.
    #[test]
    fn test_void_review_wrong_card_is_rejected() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let other_hash = CardHash::hash_bytes(b"b");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        db.insert_card(other_hash, now)?;

        let session_id = db.create_session(now)?;
        let review = sample_review(card_hash, now, 2.0);
        let review_id =
            db.insert_review_and_update_performance(session_id, &review, sample_performance(now, 2.0, 1))?;

        // Same review ID, wrong card: must fail rather than corrupt state.
        let result = db.void_review_and_restore_performance(
            review_id,
            other_hash,
            sample_performance(now, 9.0, 7),
        );
        assert!(result.is_err());

        // The review is still live and neither card was modified.
        assert_eq!(db.get_reviews_for_session(session_id)?.len(), 1);
        match db.get_card_performance(card_hash)? {
            Performance::Reviewed(rp) => assert_eq!(rp.stability, 2.0),
            Performance::New => panic!("expected card to remain reviewed"),
        }
        assert!(matches!(db.get_card_performance(other_hash)?, Performance::New));
        Ok(())
    }

    /// Closing a session that does not exist is an error, not a silent no-op.
    #[test]
    fn test_close_nonexistent_session_is_error() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        assert!(db.close_session(999, Timestamp::now()).is_err());
        Ok(())
    }

    /// Trying to delete a non-existent card returns an error.
    #[test]
    fn test_delete_nonexistent_card() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let result = db.delete_card(card_hash);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(err.to_string(), "error: Card not found");
        Ok(())
    }

    /// Delete a card and see that it is gone.
    #[test]
    fn test_delete_card() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        db.delete_card(card_hash)?;
        let result = db.get_card_performance(card_hash);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.to_string(),
            format!("error: No performance data found for card with hash {card_hash}")
        );
        Ok(())
    }

    /// A failed card deletion must not leave partial state behind.
    ///
    /// We force `delete from cards` to fail with a RESTRICT foreign key from a
    /// scratch table; the card's reviews must survive the failed deletion.
    /// The old implementation deleted reviews in a separate statement first,
    /// so they were lost even though delete_card returned an error.
    #[test]
    fn test_delete_card_failure_leaves_no_partial_state() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        let session_id = db.create_session(now)?;
        let review = sample_review(card_hash, now, 2.0);
        db.insert_review_and_update_performance(
            session_id,
            &review,
            sample_performance(now, 2.0, 1),
        )?;

        // Block deletion of this card with a restricting reference.
        db.conn.execute_batch(&format!(
            "create table blocker (card_hash text references cards (card_hash) on delete restrict);
             insert into blocker (card_hash) values ('{card_hash}');"
        ))?;

        let result = db.delete_card(card_hash);
        assert!(result.is_err());

        // The failed deletion must leave the card AND its reviews intact.
        assert!(db.card_exists(card_hash)?);
        assert_eq!(db.get_reviews_for_session(session_id)?.len(), 1);
        Ok(())
    }

    /// Deleting a card removes its reviews via the FK cascade.
    #[test]
    fn test_delete_card_cascades_to_reviews() -> Fallible<()> {
        let mut db = Database::new(":memory:")?;
        let card_hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(card_hash, now)?;
        let session_id = db.create_session(now)?;
        let review = sample_review(card_hash, now, 2.0);
        db.insert_review_and_update_performance(
            session_id,
            &review,
            sample_performance(now, 2.0, 1),
        )?;

        db.delete_card(card_hash)?;
        assert!(!db.card_exists(card_hash)?);
        assert_eq!(db.get_reviews_for_session(session_id)?.len(), 0);
        Ok(())
    }

    /// Round-trip bookmark insert/get/list/delete.
    #[test]
    fn test_bookmark_crud() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(hash, now)?;

        assert!(!db.bookmark_exists(hash)?);
        db.insert_bookmark(hash, Some("needs rephrasing".to_string()), now)?;
        assert!(db.bookmark_exists(hash)?);

        let bm = db.get_bookmark(hash)?.unwrap();
        assert_eq!(bm.card_hash, hash);
        assert_eq!(bm.note, Some("needs rephrasing".to_string()));

        let list = db.list_bookmarks()?;
        assert_eq!(list.len(), 1);

        db.update_bookmark_note(hash, None)?;
        let bm = db.get_bookmark(hash)?.unwrap();
        assert_eq!(bm.note, None);

        db.delete_bookmark(hash)?;
        assert!(!db.bookmark_exists(hash)?);
        Ok(())
    }

    /// Deleting a card cascades to its bookmark.
    #[test]
    fn test_bookmark_cascade_delete() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card(hash, now)?;
        db.insert_bookmark(hash, None, now)?;
        assert!(db.bookmark_exists(hash)?);
        db.delete_card(hash)?;
        assert!(!db.bookmark_exists(hash)?);
        Ok(())
    }

    /// rename_card_hash migrates the bookmark FK via ON UPDATE CASCADE.
    #[test]
    fn test_rename_card_hash_cascades_bookmark() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let old_hash = CardHash::hash_bytes(b"old");
        let new_hash = CardHash::hash_bytes(b"new");
        let now = Timestamp::now();
        db.insert_card(old_hash, now)?;
        db.insert_bookmark(old_hash, Some("note".to_string()), now)?;
        db.rename_card_hash(old_hash, new_hash)?;
        assert!(!db.bookmark_exists(old_hash)?);
        assert!(db.bookmark_exists(new_hash)?);
        let bm = db.get_bookmark(new_hash)?.unwrap();
        assert_eq!(bm.note, Some("note".to_string()));
        Ok(())
    }

    /// rename_card_hash fails when the new hash already exists.
    #[test]
    fn test_rename_card_hash_conflict() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let hash_a = CardHash::hash_bytes(b"a");
        let hash_b = CardHash::hash_bytes(b"b");
        let now = Timestamp::now();
        db.insert_card(hash_a, now)?;
        db.insert_card(hash_b, now)?;
        let result = db.rename_card_hash(hash_a, hash_b);
        assert!(result.is_err());
        Ok(())
    }

    /// insert_card_if_new inserts only when missing.
    #[test]
    fn test_insert_card_if_new() -> Fallible<()> {
        let db = Database::new(":memory:")?;
        let hash = CardHash::hash_bytes(b"a");
        let now = Timestamp::now();
        db.insert_card_if_new(hash, now)?;
        db.insert_card_if_new(hash, now)?; // no error on second call
        assert_eq!(db.card_hashes()?.len(), 1);
        Ok(())
    }
}
