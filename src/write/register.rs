//! `RegisterProject` — the one verb in this module that both takes an
//! idempotency key and performs a write.
//!
//! The argument for why it is served at all, and why nothing calls it in this
//! release, is in the parent module's header (`src/write.rs`).

use prost::Message as _;
use sqlx::{MySql, Transaction};
use tonic::Status;
use yadgar_dial::Deadline;

use crate::idem::{self, Claimed};
use crate::path;
use crate::pb::yadgar::common::v1::{Meta, Visibility};
use crate::pb::yadgar::project::v1::*;
use crate::pooled::Pooled;
use crate::service::{ProjectDb, REGISTER_CALLER};
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
    ///
    /// `caller` is the common name on the client certificate the listener
    /// verified, which the gRPC handler reads with
    /// [`yadgar_lifecycle::serve_tls::caller_common_name`]. It is a parameter for
    /// the reason [`ProjectDb::sync_as`] gives: over the wire there is no way to
    /// supply it except by presenting the certificate.
    pub async fn register_as(
        &self,
        caller: Option<&str>,
        req: RegisterProjectRequest,
        deadline: Deadline,
    ) -> Result<RegisterProjectResponse, Status> {
        // THE CALLER CHECK COMES BEFORE THE STORE (ADR-1007): before the path
        // is validated and before the pool is touched. (The handler's telemetry
        // has already read the Scope by then.) A refusal in `register` can
        // say whether a path is taken; a caller that is not `project` gets none
        // of those answers, and takes no connection out of the pool.
        admit(caller)?;
        self.register(req, deadline).await
    }

    /// The registration itself, for an admitted caller only.
    async fn register(
        &self,
        req: RegisterProjectRequest,
        deadline: Deadline,
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
        // It sits here on a COST argument instead: a request that cannot succeed should not take a
        // connection out of the pool and open a transaction to be told so. `local` is the root of
        // the PRIVATE class of project ids, so a project owning it becomes the nearest registered
        // ancestor of every private path in the estate and swallows all of them — see
        // `path::RESERVED_ROOT` for the whole of the argument. Registration is immutable here,
        // because rename is held back, so that door only opens one way; the guard therefore ships
        // AHEAD of the first caller of this rpc rather than behind it.
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

        // ABOVE THE TRANSACTION, on the cost and no-oracle arguments — see [`Class`].
        let class = Class::of(&req.path, &scope.user_id, &req.source_repo)?;

        let mut conn = Pooled::acquire(&self.pool, deadline).await?;
        let mut tx = conn.begin().await?;
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
               (id, version, path, display_name, status, source_repo, owner_user_id, visibility,
                created_by, updated_by)
             VALUES (?, 1, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&req.path)
        .bind(&req.display_name)
        // ACTIVE, assigned. `RegisterProjectRequest` carries no status field, so
        // there is nothing here a caller could have asked for and been refused.
        .bind(ProjectStatus::Active as i8)
        .bind(None::<&str>) // `source_repo`: a private row carries an owner instead.
        .bind(&class.owner_user_id)
        .bind(class.visibility)
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

/// The class columns a PRIVATE registration lands with — the only class this
/// rpc registers since `SyncProjects` became the one writer of org-class rows
/// (ADR-0936; the refusal is at the top of `register`).
///
/// ADR-0605 made the class structural, so `local/<account>/<path>` IS the private
/// class by construction of the id and [`crate::path::is_private_class`] reads it
/// off the path — the same predicate `ck_project_class_path` applies in the
/// engine, spelled twice on purpose so a disagreement is impossible rather than
/// unlikely.
///
/// **A PRIVATE REGISTRATION CARRYING A `source_repo` IS REFUSED, and by this
/// function rather than by the engine.** `ck_project_class` requires exactly one
/// of `source_repo` and `owner_user_id`, so the combination is already impossible
/// to store — but what a caller would receive is `sql::internal`'s
/// `INTERNAL "storage error"`, which names neither the field nor the reason.
///
/// Called ABOVE `pool.begin()`, on the cost argument `path::refuse_reserved_root`
/// makes: a request that cannot succeed should not take a connection out of the
/// pool to be told so. For the `source_repo` refusal that is the whole reason:
/// below `idem::claim` it would roll the claim back with the transaction
/// anyway. **For the ACCOUNT guard the placement is correctness** — below
/// `alias_owner` or the INSERT, a registered foreign path would answer
/// differently from an unregistered one (ADR-0954); see the guard itself.
struct Class {
    owner_user_id: String,
    visibility: i8,
}

impl Class {
    /// `source_repo` is the request's field, EMPTY MEANING ABSENT — proto3
    /// gives a bare `string` no presence bit.
    ///
    /// **AN ORG-CLASS PATH IS REFUSED FIRST: THE ONE-WRITER RULE, MADE
    /// MECHANICAL** (ADR-0705, ADR-0936). An organisation project is declared
    /// in the GitOps repository and written by `SyncProjects`; this rpc is the
    /// private class's path, so the two writers are disjoint by
    /// `ck_project_class` rather than by a convention. `FAILED_PRECONDITION`
    /// and not `INVALID_ARGUMENT`: the path is well-formed, and what refuses it
    /// is who owns its class.
    fn of(path: &str, actor: &str, source_repo: &str) -> Result<Self, Status> {
        if !crate::path::is_private_class(path) {
            return Err(Status::failed_precondition(format!(
                "{path:?} is an organisation project path. Organisation projects are declared \
                 in the GitOps repository and written by the seeder (ADR-0934, ADR-0936); \
                 RegisterProject registers a private `local/<account>/...` project only"
            )));
        }
        // THE ACCOUNT GUARD (ADR-0605's rule, ADR-0954's rendering; ledger 641).
        // A `local/<seg>/...` path registers only for the caller whose user id
        // renders `<seg>` — see `path::account_segment` for the rendering and
        // `path::is_under_own_root` for why only the canonical spelling passes.
        //
        // **ABOVE `alias_owner` AND THE INSERT IS LOAD-BEARING, NOT ONLY
        // CHEAPER.** Below either, a foreign path held as an alias would answer
        // `ALREADY_EXISTS` naming its project, and a registered one
        // `uq_project_path`'s `ALREADY_EXISTS`, while an unregistered one meets
        // this refusal — so a stranger could learn which private paths exist,
        // the oracle ADR-0954 closes. (The replay arm is not part of it: a
        // replay is keyed on the caller's OWN user and key, so it discloses
        // nothing about anyone else.) Here the answer depends on the path and
        // the caller alone, never on the store.
        //
        // `INVALID_ARGUMENT` for a user id that owns no segment, the code
        // `SyncProjects` gives a missing one. `user_id` is the gateway's
        // attestation and unbounded, so it is named and never echoed.
        let Some(seg) = crate::path::account_segment(actor) else {
            return Err(Status::invalid_argument(
                "scope.user_id is not a canonical `yadgar:user:<lowercase uuidv7>` id, so it owns \
                 no account segment and can register no private `local/<account>/...` path \
                 (ADR-0954)",
            ));
        };
        // `PERMISSION_DENIED`, the code `SyncProjects` gives the wrong
        // principal: the path is well-formed, and what refuses it is whose it
        // is. The message names the CALLER'S OWN segment and the path the
        // caller sent, and nothing the store holds.
        if !crate::path::is_under_own_root(path, seg) {
            return Err(Status::permission_denied(format!(
                "{path:?} is not under your private root \"local/{seg}\". A private project id \
                 is `local/<account>/...`, where <account> is your user id without its \
                 `yadgar:user:` prefix, spelled exactly in lowercase (ADR-0954)"
            )));
        }
        if !source_repo.is_empty() {
            return Err(Status::invalid_argument(format!(
                "source_repo was sent as {source_repo:?} for the private-class path \
                 {path:?}. A `local/...` project belongs to one account and carries an \
                 owner, never a repository — `ck_project_class` in the store admits \
                 exactly one of the two, so this row cannot exist. Omit source_repo"
            )));
        }
        Ok(Self {
            // THE CALLER, and by now the path's account segment IS the caller's:
            // the account guard above refused every other path (ADR-0954). So
            // the owner column and the path agree by construction, and the
            // gateway may read ownership off either.
            owner_user_id: actor.to_string(),
            // D12's default for the private class.
            visibility: Visibility::Private as i8,
        })
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
/// `GetProject` returns them.
///
/// **`owner_user_id` IS `actor`, THE SAME VALUE THE `INSERT` STORED.** This rpc
/// writes private rows only (ADR-0936; `Class::of` refuses an org path), and a
/// private row's owner is the caller (`Class::of`). So the answer can state the
/// owner without reading the row back, and it agrees with what `GetProject` and
/// `ListProjects` report (`rows.rs`). `team_id` and `visibility` stay empty for
/// the reason `rows.rs` gives: the table has no team column, and the class is
/// the path rather than the stored `visibility`.
fn meta_of(id: &str, version: u64, path: &str, actor: &str) -> Meta {
    Meta {
        id: id.to_string(),
        version,
        project_id: path.to_string(),
        owner_user_id: actor.to_string(),
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

/// Admit `project`'s leaf and refuse every other caller, or no caller at all
/// (ADR-1007, the gate `SyncProjects` has had since ADR-0936).
///
/// **VERIFICATION ALONE ADMITS NOBODY HERE.** Every module's client leaf
/// verifies against the same internal authority, so a `required` listener lets
/// `gateway`, `task`, `iam` and the seeder through the handshake. Without this
/// check, any of them could register a private row in any account's name its
/// own `Scope` claimed.
fn admit(caller: Option<&str>) -> Result<(), Status> {
    if caller == Some(REGISTER_CALLER) {
        return Ok(());
    }
    Err(Status::permission_denied(format!(
        "RegisterProject is callable by `project` only (ADR-1007): the client certificate \
         must name {REGISTER_CALLER:?}, and this one names {}. Every module's leaf verifies \
         against the same authority, so verification alone admits nobody here",
        caller.map_or_else(|| "no caller".to_string(), |c| format!("{c:?}"))
    )))
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
