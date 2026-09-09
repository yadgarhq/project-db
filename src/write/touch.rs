//! `TouchProjects` — the debounced `last_seen_at` flush (D52).
//!
//! Why one rpc is one transaction, and why this one carries no idempotency
//! key, is in the parent module's header (`src/write.rs`).

use tonic::Status;

use crate::path;
use crate::pb::yadgar::project::v1::*;
use crate::service::ProjectDb;
use crate::sql::{holes, internal, scope_of};

/// How many paths one `TouchProjects` may carry.
///
/// **THE CALLER CHOOSES THE SIZE OF THIS STATEMENT, WHICH IS WHY IT IS
/// BOUNDED.** D56 bounds a read for the same reason. The bucket upstream is
/// flushed periodically (D52), so a flush that has grown past this is a flush
/// that should have happened sooner, and splitting it is the caller's to do.
const MAX_TOUCH: usize = 500;

/// The first and last seconds a MariaDB `TIMESTAMP` can hold —
/// 1970-01-01T00:00:01Z to 2038-01-19T03:14:07Z.
///
/// **`FROM_UNIXTIME` ANSWERS NULL OUTSIDE THIS RANGE RATHER THAN FAILING**, and
/// `last_seen_at` is a nullable column, so a `seen_at` beyond it would write NULL
/// — erasing the value instead of advancing it, with nothing reporting anything.
/// The range is checked here so the refusal is a sentence.
///
/// The lower bound is 1 rather than 0 because a `TIMESTAMP` begins at
/// `1970-01-01 00:00:01`; whether `FROM_UNIXTIME(0)` lands inside it depends on
/// the session's time zone, and a bound that holds only in UTC is not one.
const MIN_TIMESTAMP_SECONDS: i64 = 1;
const MAX_TIMESTAMP_SECONDS: i64 = 2_147_483_647;

impl ProjectDb {
    /// Debounced upstream in Valkey and flushed periodically (D52). Never called
    /// once per request.
    ///
    /// **IT CARRIES NO IDEMPOTENCY KEY AND NEEDS NONE.** The contract gives it
    /// none, and the write is a monotonic advance of one column: applying it
    /// twice is applying it once. D9 exists for writes a replay would duplicate.
    ///
    /// **THE ADVANCE IS MONOTONIC, WHICH IS A GUARD RATHER THAN A DETAIL.**
    /// Flushes are periodic and independent, so a delayed one carrying an older
    /// `seen_at` arrives after a newer one. Writing it unconditionally would move
    /// the value BACKWARDS, and its only consumer is D47's notice expiry, which
    /// reasons in days — a project that has been active all week would age out
    /// because one late flush said it had not.
    ///
    /// **IT NEITHER BUMPS `version` NOR MOVES `updated_at`.** See `schema.rs`:
    /// `version` is D8's compare-and-set counter and a background write moving it
    /// would fail a concurrent writer for a reason no caller could see.
    ///
    /// **A PATH WITH NO ROW IS NOT AN ERROR AND IS NOT SILENT EITHER.** The
    /// response is empty by contract, so there is no channel to report it on, and
    /// refusing the batch would let one unregistered path block the flush for
    /// every other project for ever. It is counted and logged instead, because a
    /// bucket accumulating a path that resolves to nothing is a real fault
    /// upstream and the only place it can be seen is here.
    ///
    /// **THE COUNT AND THE UPDATE ARE ONE TRANSACTION**, because D5 says so.
    ///
    /// **AND THE COUNT RUNS SECOND, WHICH IS THE HALF ONE TRANSACTION DOES NOT
    /// BUY.** This used to count first, on the stated ground that one
    /// transaction was what kept the two statements describing one registry. A
    /// transaction gives ATOMICITY, not a shared view: a plain `SELECT` opens
    /// InnoDB's read view, and an `UPDATE` after it is a CURRENT read that sees
    /// the latest committed rows instead. So the two statements read two
    /// different registries while sitting in one transaction — the outcome the
    /// old comment named as the one it had ruled out.
    ///
    /// MariaDB does not answer that quietly. Measured on `mariadb:11.8.9` at the
    /// stock `REPEATABLE READ`, with a second session committing between the two
    /// statements, the `UPDATE` raises **`ERROR 1020 (HY000) Record has changed
    /// since last read in table 'project'; try restarting transaction`** —
    /// ER_CHECKREAD — and the whole flush fails. Two reachers, both measured:
    /// another `TouchProjects` naming a path this one also names, which is the
    /// ORDINARY case for a debounced flush several instances perform
    /// independently (D52); and a `RegisterProject` of a path this flush names.
    /// 1020 is neither 1205 nor 1213 and reports SQLSTATE HY000, so
    /// [`internal`] renders it `INTERNAL "storage error"` — a retryable
    /// serialisation conflict reported to the caller as a permanent fault.
    ///
    /// Counting AFTER the update takes the read view out from in front of the
    /// write. The update is then the transaction's first statement and opens no
    /// snapshot; the count is the first consistent read, so it sees the latest
    /// committed registry PLUS this transaction's own writes — which is the
    /// number the warning was always supposed to carry. Measured on the same
    /// engine and the same interleaving: no error, the update matching 2 and the
    /// count answering 2, where the old order raised 1020.
    ///
    /// **WHAT THE NEW ORDER STILL CANNOT SEE, said rather than left to be
    /// rediscovered.** A registration committed between the update and the count
    /// is counted and was not updated, so the flush stays silent about a path
    /// whose `last_seen_at` did not move. That is the benign direction: the next
    /// flush advances it and no warning cries wolf. The old order failed in the
    /// loud direction and then failed outright.
    ///
    /// **`rows_affected()` IS NOT THE NUMBER EITHER**, which is why the count is
    /// a second statement rather than deleted. The update carries the monotonic
    /// guard, so a path already holding a NEWER `last_seen_at` resolves to a
    /// project and changes nothing — counting the update's own rows would report
    /// it as a path that resolves to none. A locking `COUNT` would agree with the
    /// update exactly, and is refused for the reason ADR-0513 gives: it takes
    /// shared locks on every matched row and gap locks on every path with no row,
    /// which serialises flushes against each other and against
    /// `RegisterProject`.
    pub(crate) async fn touch(
        &self,
        req: TouchProjectsRequest,
    ) -> Result<TouchProjectsResponse, Status> {
        let _scope = scope_of(&req.scope)?;

        let seen_at = req.seen_at.as_ref().ok_or_else(|| {
            Status::invalid_argument(
                "seen_at is required. Substituting the server's clock would record when the \
                 flush arrived rather than when anything was seen, and a delayed flush would \
                 then look like recent activity",
            )
        })?;
        if seen_at.seconds < MIN_TIMESTAMP_SECONDS || seen_at.seconds > MAX_TIMESTAMP_SECONDS {
            return Err(Status::invalid_argument(format!(
                "seen_at is {} seconds from the epoch, which last_seen_at cannot hold: it is a \
                 TIMESTAMP, so the range is {MIN_TIMESTAMP_SECONDS} to {MAX_TIMESTAMP_SECONDS} \
                 (1970-01-01T00:00:01Z to 2038-01-19T03:14:07Z). Outside it the engine stores \
                 NULL, which would erase the value rather than advance it",
                seen_at.seconds
            )));
        }

        if req.paths.is_empty() {
            return Err(Status::invalid_argument(
                "paths is empty. An empty flush is a caller that has nothing to say, and \
                 answering OK to it hides a bucket that is not filling",
            ));
        }
        if req.paths.len() > MAX_TOUCH {
            return Err(Status::invalid_argument(format!(
                "paths carries {} entries and the limit is {MAX_TOUCH}",
                req.paths.len()
            )));
        }
        for path in &req.paths {
            path::validate("paths", path)?;
        }

        let paths = deduplicated(&req.paths);

        // AN ALIAS IS TOUCHED TOO. Records written before a rename still carry
        // the old path, so a bucket keyed on what a record carries holds former
        // paths — and a project that is being used every day would otherwise
        // look untouched since the day it was renamed.
        //
        // AUDIT: the interpolations are counts of `?` placeholders; every value
        // is bound.
        let holes = holes(paths.len());
        let matches = format!(
            "(path IN ({holes})
              OR id IN (SELECT project_id FROM project_alias WHERE alias_path IN ({holes})))"
        );

        let mut tx = self.pool.begin().await.map_err(internal)?;

        // THE WRITE FIRST, so that no read view stands in front of it. See this
        // function's documentation for the measurement.
        let sql = format!(
            "UPDATE project SET last_seen_at = FROM_UNIXTIME(?)
              WHERE {matches}
                AND (last_seen_at IS NULL OR last_seen_at < FROM_UNIXTIME(?))"
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(seen_at.seconds);
        for path in paths.iter().chain(paths.iter()) {
            query = query.bind(*path);
        }
        query
            .bind(seen_at.seconds)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;

        let mut counted = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM project WHERE {matches}"
        )));
        for path in paths.iter().chain(paths.iter()) {
            counted = counted.bind(*path);
        }
        let matched: i64 = counted.fetch_one(&mut *tx).await.map_err(internal)?;

        tx.commit().await.map_err(internal)?;

        if matched < paths.len() as i64 {
            tracing::warn!(
                offered = paths.len(),
                matched,
                "a TouchProjects flush named paths that resolve to no registered project. \
                 Nothing is auto-created here (D52), so those buckets will never land"
            );
        }
        Ok(TouchProjectsResponse {})
    }
}

/// The paths of one flush, as a SET — under the store's own idea of sameness.
///
/// **A BUCKET FLUSH MAY CARRY THE SAME PATH TWICE, and an `IN` list is a set**,
/// so the duplicates go. That much is bookkeeping. What makes this a function
/// rather than two lines at the call site is WHICH values count as the same one.
///
/// **THE COMPARISON IS ASCII-CASE-INSENSITIVE BECAUSE THE ENGINE'S IS.**
/// `project.path` and `project_alias.alias_path` collate `utf8mb4_general_ci`
/// (`crate::schema`, migration 4), so `path IN (…)` folds ASCII case. Migration
/// 4 is what makes that a property of the SCHEMA: before it the columns took
/// `@@collation_server`, and an engine defaulting to `utf8mb4_bin` would have
/// made this function's folding wider than the store's. A byte-wise dedup
/// therefore counts `alpha` and `ALPHA` as two while `COUNT(*)` finds the one
/// row they both name, and the caller of this function compares those two
/// numbers: the flush would report that it "named paths that resolve to no
/// registered project" when every path in it resolved. That warning exists to
/// surface a real fault upstream, and a warning that cries wolf is one nobody
/// reads. Case is the WHOLE of what the collation folds within the path grammar
/// `validate` admits — `[A-Za-z0-9._-]`, where `-`, `.` and `_` are each
/// measured UNEQUAL to nothing but themselves — so folding ASCII case is exactly
/// as wide as the engine.
///
/// **WHICH SPELLING SURVIVES IS IMMATERIAL, and that is worth saying rather than
/// leaving to be re-derived.** The survivor is bound into a predicate the engine
/// evaluates under that same collation, so either spelling reaches the same row
/// and updates the same column. Nothing downstream of here reads the value as
/// text.
///
/// Borrowed, never cloned: the survivors are bound as parameters and the request
/// outlives the statement.
fn deduplicated(paths: &[String]) -> Vec<&String> {
    let mut out: Vec<&String> = paths.iter().collect();
    // Sorted on a case-folded key so the equal ones are adjacent, which is what
    // `dedup_by` requires. `to_ascii_lowercase` allocates, and it is bounded:
    // `MAX_TOUCH` paths of at most `path::MAX_LEN` characters.
    out.sort_by_key(|p| p.to_ascii_lowercase());
    out.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SENTINEL PATH. Nothing in this crate could produce it, and it is
    /// deliberately not one of the contract's own worked examples — a test built
    /// on those can be satisfied by an implementation that special-cases the
    /// documentation.
    const P: &str = "pangolin-7c21/alpha";

    fn owned(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    /// **THE COUNT THIS FUNCTION PRODUCES IS COMPARED AGAINST ONE THE ENGINE
    /// PRODUCES**, so it has to count the way the engine counts. Two spellings
    /// of one path are one row under `uq_project_path`, so a flush carrying both
    /// offers ONE path — not two, one of which "resolves to no registered
    /// project".
    ///
    /// The literal `1` is spelled here rather than derived from anything the
    /// implementation knows (ADR-0573).
    #[test]
    fn two_ascii_case_spellings_of_one_path_are_one_path() {
        let flush = owned(&[P, &P.to_ascii_uppercase()]);
        assert_eq!(
            deduplicated(&flush).len(),
            1,
            "the store folds ASCII case, so these two name one row — counting them as two makes \
             the unmatched warning fire on a flush in which everything matched"
        );
    }

    /// The ordinary case the dedup was written for, kept so that a
    /// case-insensitive comparison cannot be mistaken for the whole of the job.
    #[test]
    fn the_same_spelling_twice_is_one_path() {
        assert_eq!(deduplicated(&owned(&[P, P])).len(), 1);
    }

    /// **THE FIXTURE THAT PINS THE SORT KEY RATHER THAN THE COMPARISON.**
    /// `dedup_by` removes only CONSECUTIVE equals, so the case-folded sort is
    /// half the fix and not a tidier spelling of it — and a two-element fixture
    /// cannot tell the two halves apart, because a byte sort leaves two
    /// spellings of one path adjacent anyway.
    ///
    /// The third path is what separates them: byte-wise, every upper-case
    /// spelling sorts before every lower-case one, so this flush byte-sorts to
    /// `ALPHA, BRAVO, alpha` and the two alphas are no longer neighbours. A
    /// dedup that folded case but sorted bytes would answer 3.
    #[test]
    fn two_spellings_of_one_path_are_still_one_when_another_path_sorts_between_them() {
        let flush = owned(&[P, &P.to_ascii_uppercase(), "PANGOLIN-7C21/BRAVO"]);
        assert_eq!(
            deduplicated(&flush).len(),
            2,
            "two projects were named, in three spellings"
        );
    }

    /// **THE FIXTURE A CASE-INSENSITIVE COMPARISON THAT WENT TOO FAR WOULD
    /// FAIL.** `-`, `.` and `_` are each measured UNEQUAL to anything but
    /// themselves under `utf8mb4_general_ci`, and two sibling projects are
    /// two rows. Collapsing them would silently drop one project's flush.
    #[test]
    fn paths_that_differ_by_more_than_case_stay_separate() {
        let flush = owned(&[P, "pangolin-7c21/bravo", "pangolin-7c21_alpha"]);
        assert_eq!(deduplicated(&flush).len(), 3);
    }

    /// WHICHEVER SPELLING SURVIVES, IT IS ONE THE CALLER SENT. The survivor is
    /// bound as a parameter, and inventing a normalised value here would be the
    /// quiet rewriting of an identity ADR-0569 refuses.
    #[test]
    fn the_survivor_is_a_spelling_the_caller_offered() {
        let shouted = P.to_ascii_uppercase();
        let flush = owned(&[P, &shouted]);
        let kept = deduplicated(&flush);
        assert_eq!(kept.len(), 1);
        assert!(
            kept[0].as_str() == P || kept[0].as_str() == shouted,
            "the survivor was {:?}, which is neither spelling the caller sent",
            kept[0]
        );
    }
}
