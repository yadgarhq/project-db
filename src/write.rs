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
//! transaction and before D9's claim. A refusal that had already claimed an
//! idempotency key would spend that key on an operation nobody performed, so the
//! caller's later retry of a DIFFERENT request under it would be refused as a
//! differing payload.
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

/// The width of `project.display_name`.
const MAX_DISPLAY_NAME: usize = 255;

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
        // THE RESERVED SEGMENT, REFUSED BEFORE THE TRANSACTION OPENS AND
        // THEREFORE BEFORE D9'S CLAIM — the same ordering the two held-back
        // verbs take, and for the same reason: a refusal that had already
        // claimed an idempotency key would spend that key on an operation nobody
        // performed. `local` is the root of the PRIVATE class of project ids, so
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
        if req.display_name.len() > MAX_DISPLAY_NAME {
            return Err(Status::invalid_argument(format!(
                "display_name is {} characters and the limit is {MAX_DISPLAY_NAME}",
                req.display_name.len()
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
        if let Some(owner) = locked_alias_owner(&mut tx, &req.path).await? {
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
    /// **THE COUNT AND THE UPDATE ARE ONE TRANSACTION**, because D5 says so and
    /// because two acquires from the pool would read two snapshots — so the
    /// number in the warning would describe a registry that no longer existed by
    /// the time the update ran.
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

        // DEDUPLICATED, because a bucket flush may carry the same path twice and
        // an `IN` list is a set. It also makes the unmatched count below mean
        // what it says.
        let mut paths: Vec<&String> = req.paths.iter().collect();
        paths.sort();
        paths.dedup();

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

        let mut counted = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM project WHERE {matches}"
        )));
        for path in paths.iter().chain(paths.iter()) {
            counted = counted.bind(*path);
        }
        let matched: i64 = counted.fetch_one(&mut *tx).await.map_err(internal)?;

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

/// Which project, if any, holds `alias` as a former path — with the row, or the
/// GAP where it would go, locked until this transaction ends.
///
/// **THE `FOR UPDATE` GUARDS NOTHING REACHABLE IN THIS RELEASE, AND IT IS NOT THE
/// SHAPE THE RENAME VERB SHOULD COPY.** Removing it kills no test here, because
/// `RenameProject` is held back and nothing else writes `project_alias` — there
/// is no concurrent writer for a plain `SELECT` to miss.
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
/// caller needs from it is the owner.
async fn locked_alias_owner(
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
