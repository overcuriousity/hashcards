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

use std::collections::HashMap;

use crate::error::Fallible;
use crate::error::fail;
use crate::types::card_hash::CardHash;
use crate::types::performance::Performance;

/// An in-memory snapshot of card performance for the current session.
///
/// Each card's latest performance is held here so that:
/// - grading can compute the new performance without an extra DB read, and
/// - undo has the previous performance readily available to restore.
///
/// Card performance is now written to the DB immediately on each grade; this
/// cache is not the source of truth — it is a fast-access mirror.
pub struct Cache {
    /// A map of card IDs to their current in-session performance.
    changes: HashMap<CardHash, Performance>,
}

impl Cache {
    /// Creates a new, empty cache.
    pub fn new() -> Self {
        Self {
            changes: HashMap::new(),
        }
    }

    /// Insert's a card performance information. If the hash is already in
    /// the cache, returns an error.
    pub fn insert(&mut self, card_hash: CardHash, performance: Performance) -> Fallible<()> {
        match self.changes.get(&card_hash) {
            Some(_) => fail(format!("Card with hash {card_hash} already in cache")),
            None => {
                self.changes.insert(card_hash, performance);
                Ok(())
            }
        }
    }

    /// Retrieve a card's performance information. If the hash is not in the
    /// cache, returns an error.
    pub fn get(&self, card_hash: CardHash) -> Fallible<Performance> {
        match self.changes.get(&card_hash) {
            Some(performance) => Ok(*performance),
            None => fail(format!("Card with hash {card_hash} not found in cache")),
        }
    }

    /// Update's a card's performance information. If the hash is not in the
    /// cache, returns an error.
    pub fn update(&mut self, card_hash: CardHash, performance: Performance) -> Fallible<()> {
        match self.changes.get_mut(&card_hash) {
            Some(p) => {
                *p = performance;
                Ok(())
            }
            None => fail(format!("Card with hash {card_hash} not found in cache")),
        }
    }

    /// Move a card's performance to a new hash after an edit renamed it.
    ///
    /// Infallible on purpose. `insert` and `update` refuse a missing or
    /// duplicate key, which is right while a session is grading; this runs
    /// after an edit has already been written to disk and committed, where
    /// there is nothing to roll back to and a hash this session never held
    /// is simply not its business. If the target hash is already present —
    /// the database declined that rename as a collision — the new value
    /// wins: the cache mirrors the file, and the database is the record.
    pub fn rekey(&mut self, old: CardHash, new: CardHash) {
        if let Some(performance) = self.changes.remove(&old) {
            self.changes.insert(new, performance);
        }
    }

    /// Forget a card the edit deleted from the corpus.
    pub fn remove(&mut self, card_hash: CardHash) {
        self.changes.remove(&card_hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::fail;
    use crate::types::date::Date;
    use crate::types::performance::ReviewedPerformance;
    use crate::types::timestamp::Timestamp;

    #[test]
    fn test_cache_insert_and_get() -> Fallible<()> {
        let mut cache = Cache::new();
        let card_hash = CardHash::hash_bytes(b"a");
        let performance = Performance::New;
        cache.insert(card_hash, performance)?;
        let retrieved = cache.get(card_hash)?;
        match retrieved {
            Performance::New => Ok(()),
            _ => fail("Expected Performance::New"),
        }
    }

    #[test]
    fn test_cache_update() -> Fallible<()> {
        let mut cache = Cache::new();
        let card_hash = CardHash::hash_bytes(b"a");
        let performance = Performance::New;
        cache.insert(card_hash, performance)?;
        let last_reviewed_at = Timestamp::now();
        let stability = 1.0;
        let difficulty = 2.0;
        let interval_raw = 0.4;
        let interval_days = 1;
        let due_date = Date::today();
        let review_count = 3;
        cache.update(
            card_hash,
            Performance::Reviewed(ReviewedPerformance {
                last_reviewed_at,
                stability,
                difficulty,
                interval_raw,
                interval_days,
                due_date,
                review_count,
            }),
        )?;
        let retrieved = cache.get(card_hash)?;
        match retrieved {
            Performance::Reviewed(rp) => {
                assert_eq!(rp.last_reviewed_at, last_reviewed_at);
                assert_eq!(rp.stability, stability);
                assert_eq!(rp.difficulty, difficulty);
                assert_eq!(rp.interval_raw, 0.4);
                assert_eq!(rp.interval_days, interval_days);
                assert_eq!(rp.due_date, due_date);
                assert_eq!(rp.review_count, review_count);
                Ok(())
            }
            _ => fail("Expected Performance::Reviewed"),
        }
    }

    #[test]
    fn test_cache_insert_duplicate() -> Fallible<()> {
        let mut cache = Cache::new();
        let card_hash = CardHash::hash_bytes(b"a");
        let performance = Performance::New;
        cache.insert(card_hash, performance)?;
        assert!(cache.insert(card_hash, performance).is_err());
        Ok(())
    }

    #[test]
    fn test_cache_get_nonexistent() -> Fallible<()> {
        let cache = Cache::new();
        let card_hash = CardHash::hash_bytes(b"a");
        assert!(cache.get(card_hash).is_err());
        Ok(())
    }

    #[test]
    fn test_cache_update_nonexistent() -> Fallible<()> {
        let mut cache = Cache::new();
        let card_hash = CardHash::hash_bytes(b"a");
        let last_reviewed_at = Timestamp::now();
        let stability = 1.0;
        let difficulty = 2.0;
        let interval_raw = 0.4;
        let interval_days = 1;
        let due_date = Date::today();
        let review_count = 3;
        let reviewed = Performance::Reviewed(ReviewedPerformance {
            last_reviewed_at,
            stability,
            difficulty,
            interval_raw,
            interval_days,
            due_date,
            review_count,
        });
        let res = cache.update(card_hash, reviewed);
        assert!(res.is_err());
        Ok(())
    }

    /// A rename moves the performance to the new hash and forgets the old
    /// one: a session that kept both would answer `get` for a card that no
    /// file contains any more.
    #[test]
    fn test_cache_rekey_moves_the_performance() -> Fallible<()> {
        let mut cache = Cache::new();
        let old = CardHash::hash_bytes(b"old");
        let new = CardHash::hash_bytes(b"new");
        cache.insert(old, Performance::New)?;
        cache.rekey(old, new);
        assert!(cache.get(new)?.is_new());
        assert!(cache.get(old).is_err());
        Ok(())
    }

    /// Rekeying a hash the session never held is a no-op, not an error: an
    /// edit renames cards across a whole file, and a session may hold only
    /// some of them.
    #[test]
    fn test_cache_rekey_of_an_absent_hash_is_a_noop() {
        let mut cache = Cache::new();
        let old = CardHash::hash_bytes(b"old");
        let new = CardHash::hash_bytes(b"new");
        cache.rekey(old, new);
        assert!(cache.get(new).is_err());
    }

    #[test]
    fn test_cache_remove_forgets_the_card() -> Fallible<()> {
        let mut cache = Cache::new();
        let hash = CardHash::hash_bytes(b"a");
        cache.insert(hash, Performance::New)?;
        cache.remove(hash);
        assert!(cache.get(hash).is_err());
        Ok(())
    }

    #[test]
    fn test_cache_iter() -> Fallible<()> {
        let mut cache = Cache::new();
        let card_hash = CardHash::hash_bytes(b"a");
        let performance = Performance::New;
        cache.insert(card_hash, performance)?;
        let mut iter = cache.changes.iter();
        let (key, value): (&CardHash, &Performance) = iter.next().unwrap();
        assert_eq!(*key, card_hash);
        assert!(value.is_new());
        assert!(iter.next().is_none());
        Ok(())
    }
}
