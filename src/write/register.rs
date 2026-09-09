//! `RegisterProject` — the one verb in this module that both takes an
//! idempotency key and performs a write.
//!
//! The argument for why it is served at all, and why nothing calls it in this
//! release, is in the parent module's header (`src/write.rs`).

use prost::Message as _;
use sqlx::{MySql, Transaction};
use tonic::Status;

use crate::idem::{self, Claimed};
use crate::path;
use crate::pb::yadgar::common::v1::Meta;
use crate::pb::yadgar::project::v1::*;
use crate::service::ProjectDb;
use crate::sql::{internal, scope_of};

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
