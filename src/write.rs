//! `RegisterProject` and `TouchProjects` — and the two verbs this release
//! REFUSES in as many words.
//!
//! One RPC is one transaction (D5), and the D9 claim is taken inside it, so a
//! write and the record that it happened commit together or not at all.
//!
//! # `RenameProject` and `ArchiveProject` answer `UNIMPLEMENTED`
//!
//! **BOTH ARE HELD BACK ON A DATA ARGUMENT RATHER THAN AN EFFORT ONE.** A rename
//! is an alias and never a rewrite (D53), which keeps every stored
//! `Meta.project_id` VALID — but it does not keep it CURRENT. Every memory, wiki
//! page, ADR and task already stamped with the old path goes on carrying it, and
//! the job that would retag them does not exist. So a rename shipped today
//! silently orphans records instead of moving them: they resolve, they are
//! readable, and no view of the new path contains them. Archiving has the same
//! shape one step earlier — an instance must be able to retag before anything is
//! archived, or the archive is the moment the records stop being findable.
//!
//! **REFUSED, NEVER OMITTED.** A gRPC service that silently lacks a contracted
//! method answers `UNIMPLEMENTED` from tonic's own fallback, with no reason in
//! it, and a caller cannot tell that from a version skew or a bad route. These
//! refuse with the reason written out, which is the estate's existing shape —
//! `SetInheritedSetting` sat `UNIMPLEMENTED` with a stated reason for the same
//! kind of ordering constraint. Both come back with the retag job, together.
//!
//! **THE REFUSAL IS THE FIRST THING EITHER HANDLER DOES**, before any
//! transaction — so neither ever ATTEMPTS a claim, and there is no D9 row to
//! reason about. That is what makes an idempotency key offered to a held-back
//! verb still free afterwards.
//!
//! It is not the ORDER that buys it, and this file used to say it was. The claim
//! is written inside the caller's transaction (`src/idem.rs`), so a refusal
//! BELOW it rolls the claim back with everything else and spends no key either —
//! `register` refuses that way twice, twenty lines apart, and
//! `tests/idempotency.rs::a_failed_write_leaves_no_claim_behind` is the proof.
//! The invariant is guaranteed by transaction atomicity. Refusing early is a
//! COST argument: a request that cannot succeed should not take a connection out
//! of the pool to be told so.
//!
//! # `RegisterProject` has no caller in this release
//!
//! Organisation-level projects are defined by GitOps rather than created on
//! demand (D43, D52), so this rpc is not the creation path for them. It is
//! implemented, tested and served because the contract declares it and because
//! the store has to be able to hold a registration at all — but nothing in the
//! estate calls it yet, and that is deliberate rather than an oversight.
//!
//! # The one namespace rule that spans two tables
//!
//! An alias and a live path share one namespace and no single constraint can say
//! so: two tables cannot hold one unique index between them. So the rule lives
//! here — nothing registers at a path that is a former path of something else.

use prost::Message as _;
use sqlx::{MySql, Transaction};
use tonic::Status;

use crate::idem::{self, Claimed};
use crate::path;
use crate::pb::yadgar::common::v1::Meta;
use crate::pb::yadgar::project::v1::*;
use crate::service::ProjectDb;
use crate::sql::{holes, internal, scope_of};

/// The width of `project.display_name`, IN CHARACTERS, because that is what
/// stores it.
///
/// **CHARACTERS AND NOT BYTES.** The column is `display_name VARCHAR(255)` on
/// `CHARSET=utf8mb4` (`crate::schema`, migration 1), and `VARCHAR(n)` in utf8mb4
/// bounds CHARACTERS. This bound was checked with `String::len`, which counts
/// BYTES, so the two disagreed by up to four to one. Measured on
/// `mariadb:11.8.9` — the image this repository's README stands up — against the
/// column exactly as the migration declares it, at the stock `sql_mode`
/// (`STRICT_TRANS_TABLES,…`):
///
/// | display_name    | characters | bytes | outcome                                          |
/// | --------------- | ---------- | ----- | ------------------------------------------------ |
/// | 255 × `l`       | 255        | 255   | stored                                           |
/// | 256 × `l`       | 256        | 256   | `ERROR 1406 Data too long for column 'display_name'` |
/// | 255 × `U+1F600` | 255        | 1020  | stored                                           |
/// | 256 × `U+1F600` | 256        | 1024  | `ERROR 1406 Data too long for column 'display_name'` |
/// | 64 × `U+1F600`  | 64         | 256   | stored                                           |
///
/// So the byte check was wrong in ONE direction, and that is worth stating
/// rather than leaving a reader to assume the symmetric case. A value passing a
/// 255-BYTE test can never exceed 255 characters, so nothing the column refuses
/// ever reached it — unlike `iam`'s `MAX_LABEL_BYTES`, which was also off by one
/// and did admit a value the column refused. What it did was REFUSE: the last
/// row of the table is a sixty-four-character name the column stores without
/// complaint, turned away as "255 characters" by a message counting something
/// else. A caller naming a project in Persian, Japanese or emoji met a limit a
/// quarter of the one the schema declares.
///
/// **THE NUMBER IS THE COLUMN'S, AND THAT COUPLING IS DELIBERATE**, the same
/// argument `iam`'s `MAX_LABEL_CHARS` makes: a `VARCHAR` width does not drift on
/// its own, it is a declaration in a migration somebody edits. Refusing here
/// makes the outcome this service's own and independent of a `sql_mode` it
/// neither sets nor checks — under `sql_mode = ''` the same INSERT stores a
/// CLIPPED 255-character name and reports success. If the column widens, this
/// constant is what has to move with it.
///
/// **`path::MAX_LEN` IS LEFT COUNTING BYTES AND THAT IS NOT THE SAME DEFECT.**
/// It guards the same utf8mb4 width, but `path::validate` admits only
/// `[A-Za-z0-9._-]`, where a character is one byte — and its length check runs
/// BEFORE the value is echoed into a refusal, which is a bound on how long a log
/// line a caller can ask for. That one is a byte question and stays a byte
/// check.
const MAX_DISPLAY_NAME_CHARS: usize = 255;

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

/// What both held-back verbs say, in one place so the two cannot drift into
/// giving different reasons for one decision.
const HELD_BACK: &str = "this release does not implement it, and the reason is the records rather \
     than the code. Every memory, wiki page, ADR and task already stamped with a project path goes \
     on carrying it, and the job that retags them does not exist yet — so moving or archiving a \
     project today would leave those records readable, valid, and absent from every view of the \
     project they belong to. RenameProject and ArchiveProject return together with that retag job.";

impl ProjectDb {
    /// Registration is an ADMINISTRATIVE act — GitOps or CLI, like configuration
    /// (D43). Agents resolve and read; they never mint a partition key (D52).
    pub(crate) async fn register(
        &self,
        req: RegisterProjectRequest,
    ) -> Result<RegisterProjectResponse, Status> {
        let scope = scope_of(&req.scope)?;
        path::validate("path", &req.path)?;
        // THE RESERVED SEGMENT, REFUSED BEFORE THE TRANSACTION OPENS — and NOT
        // because moving it below D9's claim would spend the caller's key. That
        // is what this comment used to say, and the statement twenty lines below
        // refutes it: the alias check refuses AFTER the claim, rolls the
        // transaction back, and leaves the key free —
        // `tests/idempotency.rs::a_failed_write_leaves_no_claim_behind` is the
        // assertion, and `tests/registry.rs` says the same of this very guard,
        // that moving it under the claim leaves that test green. The claim is
        // written inside the caller's transaction (`src/idem.rs`), so what keeps
        // an unperformed operation from spending a key is transaction ATOMICITY,
        // at every refusal in this function.
        //
        // It sits here on a COST argument instead: a request that cannot succeed
        // should not take a connection out of the pool and open a transaction to
        // be told so. `local` is the root of the PRIVATE class of project ids, so
        // a project owning it becomes the nearest registered ancestor of every
        // private path in the estate and swallows all of them — see
        // `path::RESERVED_ROOT` for the whole of the argument. Registration is
        // immutable here, because rename is held back, so that door only opens
        // one way; the guard therefore ships AHEAD of the first caller of this
        // rpc rather than behind it.
        path::refuse_reserved_root("path", &req.path)?;
        // EMPTY IS LEGITIMATE. A display name is presentation, and a project
        // whose name is its path is a perfectly ordinary registration — the path
        // is the identity and it is already required. What is refused is a value
        // wider than the column, so that the failure is a sentence rather than a
        // truncation.
        //
        // COUNTED IN CHARACTERS, because the column is. `chars().count()` and
        // not `len()`: a `char` is a Unicode scalar and maps one-to-one onto a
        // utf8mb4 character, where graphemes would under-count. See
        // [`MAX_DISPLAY_NAME_CHARS`].
        let display_name_chars = req.display_name.chars().count();
        if display_name_chars > MAX_DISPLAY_NAME_CHARS {
            return Err(Status::invalid_argument(format!(
                "display_name is {display_name_chars} characters and the limit is \
                 {MAX_DISPLAY_NAME_CHARS}"
            )));
        }

        let mut tx = self.pool.begin().await.map_err(internal)?;
        if let Claimed::Replay(original) = idem::claim(
            &mut tx,
            scope,
            "RegisterProject",
            req.idempotency.as_ref(),
            || req.payload(),
        )
        .await?
        {
            tx.rollback().await.map_err(internal)?;
            return Ok(original);
        }

        // A PATH THAT IS ALREADY A FORMER PATH IS TAKEN. Registering there would
        // make one path mean two things — the split-corpus failure D52 is about,
        // arriving through the alias table instead of through a typo.
        if let Some(owner) = alias_owner(&mut tx, &req.path).await? {
            tx.rollback().await.map_err(internal)?;
            return Err(Status::already_exists(format!(
                "{:?} is a former path of the project {owner} and still resolves to it (D53). \
                 Registering a project there would make one path mean two things",
                req.path
            )));
        }

        // UUIDv7: time-ordered, so keyset pagination and index locality behave
        // (D42). The URN is what leaves this service; the raw uuid never does.
        let id = format!("yadgar:project:{}", uuid::Uuid::now_v7());
        let inserted = sqlx::query(
            "INSERT INTO project
               (id, version, path, display_name, status, created_by, updated_by)
             VALUES (?, 1, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&req.path)
        .bind(&req.display_name)
        // ACTIVE, assigned. `RegisterProjectRequest` carries no status field, so
        // there is nothing here a caller could have asked for and been refused.
        .bind(ProjectStatus::Active as i8)
        .bind(&scope.user_id)
        .bind(&scope.user_id)
        .execute(&mut *tx)
        .await;

        if let Err(e) = inserted {
            tx.rollback().await.map_err(internal)?;
            // `uq_project_path` is the only unique index on this table, so a
            // duplicate here is one thing and can be named. It is the ENGINE
            // rather than a check that decides it, which is what makes the
            // answer right under two concurrent registrations of one path.
            if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
                return Err(Status::already_exists(format!(
                    "a project is already registered at {:?}",
                    req.path
                )));
            }
            return Err(internal(e));
        }

        let response = RegisterProjectResponse {
            meta: Some(meta_of(&id, 1, &req.path, &scope.user_id)),
        };
        idem::record(&mut tx, scope, req.idempotency.as_ref(), &response).await?;
        tx.commit().await.map_err(internal)?;
        Ok(response)
    }

    /// Held back until records can be retagged — see this module's header.
    ///
    /// The request is not validated first and no transaction is opened. There is
    /// nothing to validate a request AGAINST when no request of this shape can
    /// succeed, and an `INVALID_ARGUMENT` here would tell a caller to correct a
    /// value that would then be refused anyway.
    pub(crate) async fn rename(
        &self,
        _req: RenameProjectRequest,
    ) -> Result<RenameProjectResponse, Status> {
        Err(Status::unimplemented(format!("RenameProject: {HELD_BACK}")))
    }

    /// Held back until records can be retagged — see this module's header.
    pub(crate) async fn archive(
        &self,
        _req: ArchiveProjectRequest,
    ) -> Result<ArchiveProjectResponse, Status> {
        Err(Status::unimplemented(format!(
            "ArchiveProject: {HELD_BACK}"
        )))
    }

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
/// `project.path` and `project_alias.alias_path` are declared `utf8mb4` with no
/// `COLLATE` (`crate::schema`), so they take `utf8mb4_uca1400_ai_ci` — measured
/// on MariaDB 11.8 — and `path IN (…)` folds ASCII case. A byte-wise dedup
/// therefore counts `alpha` and `ALPHA` as two while `COUNT(*)` finds the one
/// row they both name, and the caller of this function compares those two
/// numbers: the flush would report that it "named paths that resolve to no
/// registered project" when every path in it resolved. That warning exists to
/// surface a real fault upstream, and a warning that cries wolf is one nobody
/// reads. Case is the WHOLE of what the collation folds within the path grammar
/// `validate` admits — `[A-Za-z0-9._-]`, where `-`, `.` and `_` are each
/// measured UNEQUAL to nothing but themselves — so folding ASCII case is exactly
/// as wide as the engine, and no `COLLATE` migration is required to make it so.
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

/// The `Meta` a mutating rpc answers with.
///
/// **EXHAUSTIVE, NEVER `..Default::default()`.** A rest pattern on a generated
/// struct compiles for ever and silently leaves whatever the contract adds next
/// at its zero value. Spelling the fields out makes a contract bump a compile
/// error somebody has to answer.
///
/// **THE TIMESTAMPS ARE ABSENT HERE, AND THAT IS A STATEMENT RATHER THAN AN
/// OMISSION.** They are the engine's `CURRENT_TIMESTAMP`, so reporting them would
/// mean reading the row back for a value no caller of a write has asked for.
/// `GetProject` returns them. `owner_user_id`, `team_id` and `visibility` are
/// empty for the reason `rows.rs` gives: a project is the partition key, not a
/// record inside one, and D12's ladder has nothing to say about it.
fn meta_of(id: &str, version: u64, path: &str, actor: &str) -> Meta {
    Meta {
        id: id.to_string(),
        version,
        project_id: path.to_string(),
        owner_user_id: String::new(),
        team_id: String::new(),
        visibility: 0,
        created_by: actor.to_string(),
        updated_by: actor.to_string(),
        created_at: None,
        updated_at: None,
        deleted_at: None,
        derived_from: Vec::new(),
    }
}

/// Which project, if any, holds `alias` as a former path.
///
/// **THIS IS A NON-LOCKING READ GATING THE `INSERT` ABOVE IT, WHICH IS A RACE
/// THIS RELEASE HAS NO WRITER FOR — and the reason matters, because the reason
/// the comment used to give was false.** It said "`RenameProject` is held back
/// and nothing else writes `project_alias`". `tests/support/mod.rs`'s
/// `seed_alias` writes it, on its own connection, and says in as many words that
/// it exists because no rpc can. A premise phrased as "nothing writes this" is
/// the premise `iam-db#36` found false in its own repository, fifty lines below
/// the comment that made it.
///
/// The claim that survives the grep is narrower and is the one that holds: no
/// SERVED rpc writes `project_alias` in this release, so nothing a caller can
/// reach commits an alias between this read and the `INSERT`. A fixture writing
/// one before or after a `register` is sequential with it and cannot land inside
/// the window. When `RenameProject` returns, the window opens, and the read
/// alone will no longer be enough.
///
/// **THE FIX THE SIBLING REPOSITORIES USE IS THE WRONG ONE HERE, which is why
/// this is described rather than closed.** The borrowed shape moves the
/// predicate into the write under `LOCK IN SHARE MODE`, and it is free only
/// where the row EXISTS. Here the common path is the ABSENT alias, so a locking
/// read takes a gap lock on nothing — the construction the paragraphs below
/// measure and ADR-0513 forbids, and the one a previous revision of this
/// function deliberately deleted. The shape that closes it is the INSERT-FIRST
/// one named at the end of this comment, and it belongs to the verb that opens
/// the window rather than to this release.
///
/// An earlier revision of this comment said the lock was "a claim on the
/// ABSENCE" and offered it as the statement the rename verb should return to.
/// **That is measurably false and ADR-0513 already ruled it out.** An InnoDB gap
/// lock on a non-existent row is purely INHIBITIVE: it blocks an INSERT into the
/// gap and does NOT exclude another transaction's identical gap lock. Measured
/// on MariaDB 11.8, the deployed engine — a second session's identical
/// `SELECT … FOR UPDATE` on the same absent key returns immediately, a foreign
/// INSERT blocks and times out with 1205, and two symmetric gap-lock-then-insert
/// transactions DEADLOCK with 1213. ADR-0513 puts it plainly: taking it is
/// strictly worse than not taking it, because each transaction's gap lock then
/// blocks the other's INSERT, turning a race into a deadlock.
///
/// **The aggravating detail**: `sql::internal` maps 1213 to `ABORTED "retry"`, so
/// a deterministic deadlock would retry for ever rather than surface.
///
/// **WHEN `RenameProject` RETURNS, IT USES THE INSERT-FIRST SHAPE** this
/// repository already uses twice — `idem::claim`, and `register`'s reliance on
/// `uq_project_path`. An INSERT takes a real record lock rather than a gap lock,
/// and the unique violation IS the signal. ADR-0513 names that shape as the
/// legitimate alternative.
///
/// So the `FOR UPDATE` is GONE rather than kept-and-disclaimed. A lock whose own
/// doc comment says it guards nothing is the next reader's trap: they keep it
/// because it looks deliberate. The read is a plain lookup, because what the
/// caller needs from it is the owner — and the NAME says so too, for the same
/// reason: `locked_alias_owner` outlived the lock it was named for, and a
/// function whose name promises a lock it does not take is the same trap in the
/// call site instead of in the comment.
async fn alias_owner(
    tx: &mut Transaction<'_, MySql>,
    alias: &str,
) -> Result<Option<String>, Status> {
    sqlx::query_scalar::<_, String>("SELECT project_id FROM project_alias WHERE alias_path = ?")
        .bind(alias)
        .fetch_optional(&mut **tx)
        .await
        .map_err(internal)
}

/// The bytes D9's fingerprint is taken over.
///
/// Stated as "clear the two, keep the rest" rather than as a list of the fields
/// to hash. The list would be the same today and would silently stop being right
/// the day the contract grows a field: a new one nobody added here would be
/// omitted from the digest, and a request differing only in it would be replayed.
/// The exclusions are what this module has actually decided about — see the
/// header of `src/idem.rs`.
///
/// ONE IMPLEMENTATION, because `RegisterProject` is the only rpc here that both
/// takes a key and performs a write. The two that are held back never reach a
/// claim.
trait Payload {
    fn payload(&self) -> Vec<u8>;
}

impl Payload for RegisterProjectRequest {
    fn payload(&self) -> Vec<u8> {
        let mut canonical = self.clone();
        canonical.scope = None;
        canonical.idempotency = None;
        canonical.encode_to_vec()
    }
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
    /// themselves under `utf8mb4_uca1400_ai_ci`, and two sibling projects are
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
