//! Bounded maintenance of quota-charged negative outcomes.
use std::collections::BTreeMap;

use redb::{ReadableDatabase as _, ReadableTable as _};

use super::{
    META, OUTCOMES, Outcome, OutcomeClass, QUOTA, Store, StoreError, decode, encode, storage,
};

impl Store {
    /// Reclaims at most 256 expired outcomes in a cursor-bounded scan.
    /// Terminal outcomes and retirement tombstones are never time-evicted.
    ///
    /// # Errors
    /// A failed store transaction. Accounting is released with row deletion.
    pub fn prune_expired_outcomes(&self, now: u64) -> Result<usize, StoreError> {
        let tx = self.db.begin_write().map_err(storage)?;
        let cursor: String = tx
            .open_table(META)
            .map_err(storage)?
            .get("outcome_prune_cursor")
            .map_err(storage)?
            .map(|value| decode(value.value()))
            .transpose()?
            .unwrap_or_default();
        let mut next = String::new();
        let mut stale = Vec::new();
        {
            let mut outcomes = tx.open_table(OUTCOMES).map_err(storage)?;
            let mut scanned = 0;
            let mut last = String::new();
            for entry in outcomes.range(cursor.as_str()..).map_err(storage)? {
                let (key, value) = entry.map_err(storage)?;
                if key.value() <= cursor.as_str() {
                    continue;
                }
                if scanned == 256 {
                    next = last;
                    break;
                }
                last = key.value().to_owned();
                scanned += 1;
                let outcome: Outcome = decode(value.value())?;
                if outcome.class != OutcomeClass::Terminal && outcome.until <= now {
                    stale.push(key.value().to_owned());
                }
            }
            for key in &stale {
                outcomes.remove(key.as_str()).map_err(storage)?;
                tx.open_table(QUOTA)
                    .map_err(storage)?
                    .remove(format!("outcome:{key}").as_str())
                    .map_err(storage)?;
            }
        }
        tx.open_table(META)
            .map_err(storage)?
            .insert("outcome_prune_cursor", encode(&next)?.as_slice())
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(stale.len())
    }

    /// Counts current per-item/target refusals by stable code for authenticated status.
    ///
    /// # Errors
    /// A failed store read.
    pub fn active_outcome_counts(&self, now: u64) -> Result<BTreeMap<String, usize>, StoreError> {
        let tx = self.db.begin_read().map_err(storage)?;
        let table = tx.open_table(OUTCOMES).map_err(storage)?;
        let mut counts = BTreeMap::new();
        for entry in table.iter().map_err(storage)? {
            let (_, value) = entry.map_err(storage)?;
            let outcome: Outcome = decode(value.value())?;
            if outcome.until > now {
                *counts.entry(outcome.code).or_default() += 1;
            }
        }
        Ok(counts)
    }
}
