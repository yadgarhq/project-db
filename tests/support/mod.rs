//! The shared fixture.
//!
//! Each test binary gets its own database, named by the caller, so the suite
//! parallelises without two tests racing on one schema.
//!
//! # Every fixture value is a sentinel
//!
//! **NOT ONE PATH HERE IS `quinyx/forecast` OR `quinyx/qwfm/forecast`.** Those
//! are the CONTRACT'S OWN worked examples, so a suite built on them can be
//! satisfied by an implementation that special-cases the documentation, and a
//! reader cannot tell an assertion about behaviour from an assertion about a
//! comment. [`ROOT`] is a token nothing in this crate could produce.
//!
//! **THE CAST IS SIBLINGS, NOT A SINGLE CHAIN.** [`A`] and [`B`] are siblings
//! under one root, so neither is an ancestor of the other — otherwise a subtree
//! query could quietly satisfy a test that is really about equality, and an
//! ancestor walk could satisfy a test that is really about an exact match.
#![allow(dead_code)]

use sqlx::{Connection, MySqlPool, Row};
use tonic::{Request, Status};
use yadgar_project_db::pb::yadgar::common::v1::{Idempotency, Scope, Visibility};
use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;
use yadgar_project_db::pb::yadgar::project::v1::*;
use yadgar_project_db::{schema, service::ProjectDb};

/// The sentinel root every fixture path hangs off.
pub const ROOT: &str = "pangolin-7c21";

/// Two siblings under [`ROOT`]. Neither is an ancestor of the other.
pub const A: &str = "pangolin-7c21/alpha";
pub const B: &str = "pangolin-7c21/bravo";

/// A service inside [`A`], for the depth the hierarchy is FOR (D53).
pub const A_DEEP: &str = "pangolin-7c21/alpha/gamma";

pub const U1: &str = "u1";
pub const U2: &str = "u2";

/// The repository an ORG-class fixture row names as the governor of its
/// namespace, when the test is not about the value.
///
/// **IT IS NOT DERIVABLE FROM ANY FIXTURE PATH, and that is the property it
/// exists for.** `source_repo` used to BE the row's own path, and the whole point
/// of the field is that the two differ — a namespace anchor's path names a
/// namespace while the repository governing it is one beneath. A default equal to
/// the path would let an assertion about `ResolveProject.source_repo` be satisfied
/// by an implementation returning `resolved_path`, and nothing would distinguish
/// the two. So it hangs off [`ROOT`] to stay plausible and ends in a segment no
/// fixture path uses.
pub const FIXTURE_REPO: &str = "pangolin-7c21/seed-repo";

/// The `source_repo` a fixture sends for *path* when the test is not about it.
///
/// **STATED HERE RATHER THAN IMPORTED FROM `write::register`**, the same argument
/// [`class_of`] makes: a fixture that borrowed the derivation could not fail when
/// the derivation is wrong. The rule is the contract's own — REQUIRED for the org
/// class, ABSENT for the private class, where absence is the empty string because
/// proto3 gives a bare `string` no presence bit.
pub fn default_source_repo(path: &str) -> &'static str {
    if is_private_path(path) {
        ""
    } else {
        FIXTURE_REPO
    }
}

/// The pool every fixture opens.
///
/// **A fixture that says nothing runs at sqlx's default of ten, which is a size
/// production never ships.** `DB_MAX_CONNECTIONS` defaults to eight and D80 made
/// it operator-settable downwards, so the sizes that matter are small ones. Four
/// is above `store`'s `MIN_CONNECTIONS` of two, so migration keeps the two
/// connections it holds at once.
// NOT `DEFAULT_POOL_SIZE`: the ADR-0569 gate forbids a `DEFAULT_*` constant
// (ledger 965, census) because the name itself asserts a fallback exists.
// This one does not fall back to anything — every fixture states it
// explicitly, below — so it is named for what it IS instead.
const FIXTURE_POOL_SIZE: u32 = 4;

pub fn dsn() -> String {
    std::env::var("YADGAR_TEST_DSN")
        .expect("YADGAR_TEST_DSN is unset; these tests assert what a real MariaDB does")
}

/// The DSN with any trailing `/database` removed.
///
/// Split on the first `/` AFTER the scheme, never the last one anywhere: a
/// rsplit finds the second slash of `mysql://` when the DSN names no database,
/// and silently builds `mysql://<db>` — a URL with the database in the host
/// position, which fails to connect for a reason nothing in the error mentions.
fn base() -> String {
    let dsn = dsn();
    let after_scheme = dsn.find("://").map(|i| i + 3).unwrap_or(0);
    match dsn[after_scheme..].find('/') {
        Some(i) => dsn[..after_scheme + i].to_string(),
        None => dsn,
    }
}

/// A service and the pool behind it, dropped and recreated per test.
///
/// The POOL is exposed deliberately: a few assertions are about what is IN a
/// column rather than about what an rpc answered, and "assigned ACTIVE" is only
/// distinguishable from "took the caller's word and happened to agree" by
/// reading the column. Reaching around the service for an assertion is honest;
/// reaching around it in the code under test would not be.
pub struct World {
    pub db: ProjectDb,
    pub pool: MySqlPool,
}

impl World {
    pub async fn fresh(name: &str) -> Self {
        let mut root = sqlx::MySqlConnection::connect(&dsn())
            .await
            .expect("connect");
        for stmt in [
            format!("DROP DATABASE IF EXISTS {name}"),
            format!("CREATE DATABASE {name}"),
        ] {
            // AUDIT: `name` is a literal in the calling test file.
            sqlx::raw_sql(sqlx::AssertSqlSafe(stmt))
                .execute(&mut root)
                .await
                .expect("ddl");
        }
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            // STATED, never inherited. See [`FIXTURE_POOL_SIZE`].
            .max_connections(FIXTURE_POOL_SIZE)
            .connect(&format!("{}/{name}", base()))
            .await
            .expect("pool");
        // The wait a chart would render. Stated in the TEST because
        // `yadgar-store` has no default for it any more (ADR-0569, ledger 814).
        let lock = yadgar_store::migrate::LockOptions::new(60).expect("60 seconds is a wait");
        yadgar_store::migrate::apply(&pool, &schema::migrations().expect("set"), &lock)
            .await
            .expect("migrate");
        Self {
            db: ProjectDb::new(pool.clone()),
            pool,
        }
    }

    /// A scope stating everything a `Scope` carries.
    ///
    /// **THE LITERAL IS EXHAUSTIVE rather than taking `..Default::default()`, and
    /// that is a tripwire.** Spreading a default here would silently absorb the
    /// next field the contract adds — including one a read path must consult.
    ///
    /// **`owner_reads_own_record` IS `None`, AND THAT IS THE POINT OF STATING
    /// IT.** `yadgar/common/v1` says a `-db` is in ADR-0522's ENFORCING state
    /// exactly when it READS that field, and must then refuse an unset one. This
    /// module has no visibility ladder, no team axis and no owner for the setting
    /// to widen, so it reads nothing and stays in the PRE-ENFORCEMENT state — see
    /// `src/service.rs`. Passing `None` on every call is what proves that: the
    /// day someone copies a sibling's `read_predicate` in here, every test in
    /// this suite turns red.
    pub fn scope(&self, project: &str, user: &str) -> Option<Scope> {
        Some(Scope {
            user_id: user.into(),
            project_id: project.into(),
            team_ids: Vec::new(),
            instance_id: "i-1".into(),
            request_id: "r-1".into(),
            owner_reads_own_record: None,
        })
    }

    pub async fn try_register(
        &self,
        path: &str,
        display_name: &str,
    ) -> Result<RegisterProjectResponse, Status> {
        self.register_as(path, display_name, U1, None).await
    }

    /// A registration whose `source_repo` is [`default_source_repo`]'s.
    ///
    /// **THE DEFAULT IS A CONVENIENCE FOR TESTS THAT ARE NOT ABOUT THE FIELD, AND
    /// EVERY TEST THAT IS ABOUT IT USES [`World::register_with_repo`] INSTEAD.**
    /// `source_repo` is REQUIRED for an org registration, so a harness with no
    /// default would make three dozen unrelated tests state a repository they do
    /// not care about — but a default is also a door around the requirement, and a
    /// test of the requirement driven through this function would stay green with
    /// the guard deleted. The two doors are separate for that reason.
    pub async fn register_as(
        &self,
        path: &str,
        display_name: &str,
        user: &str,
        key: Option<&str>,
    ) -> Result<RegisterProjectResponse, Status> {
        self.register_request(path, display_name, user, key, default_source_repo(path))
            .await
    }

    /// A registration stating `source_repo` EXACTLY, including empty.
    ///
    /// The door every assertion about the field goes through. Empty is a request a
    /// caller can really send — proto3 gives a bare `string` no presence bit, so
    /// `""` is what an omitted field decodes to — which is why this takes a `&str`
    /// rather than an `Option`.
    pub async fn register_with_repo(
        &self,
        path: &str,
        display_name: &str,
        source_repo: &str,
    ) -> Result<RegisterProjectResponse, Status> {
        self.register_request(path, display_name, U1, None, source_repo)
            .await
    }

    /// The one `RegisterProjectRequest` literal in this fixture.
    ///
    /// **EXHAUSTIVE, NEVER `..Default::default()`** — the argument
    /// [`World::scope`] makes, and the one the v1.15.0 bump proved: the missing
    /// `source_repo` was a compile error here, which is what a rest pattern would
    /// have turned into a silently empty field.
    async fn register_request(
        &self,
        path: &str,
        display_name: &str,
        user: &str,
        key: Option<&str>,
        source_repo: &str,
    ) -> Result<RegisterProjectResponse, Status> {
        self.db
            .register_project(Request::new(RegisterProjectRequest {
                idempotency: key.map(|k| Idempotency { key: k.into() }),
                scope: self.scope(path, user),
                path: path.into(),
                display_name: display_name.into(),
                source_repo: source_repo.into(),
            }))
            .await
            .map(|r| r.into_inner())
    }

    /// Register and answer with the id, which is what most tests want.
    pub async fn register(&self, path: &str) -> String {
        self.try_register(path, path)
            .await
            .expect("register")
            .meta
            .expect("meta")
            .id
    }

    pub async fn resolve(&self, candidate: &str) -> Result<ResolveProjectResponse, Status> {
        self.db
            .resolve_project(Request::new(ResolveProjectRequest {
                scope: self.scope(candidate, U1),
                candidate_path: candidate.into(),
            }))
            .await
            .map(|r| r.into_inner())
    }

    pub async fn get(&self, path: &str) -> Result<Project, Status> {
        self.db
            .get_project(Request::new(GetProjectRequest {
                scope: self.scope(path, U1),
                path: path.into(),
            }))
            .await
            .map(|r| r.into_inner().project.expect("project"))
    }

    pub async fn list(&self, under: &str) -> Vec<Project> {
        self.list_page(under, None, 0, "")
            .await
            .expect("list")
            .projects
    }

    pub async fn list_page(
        &self,
        under: &str,
        status: Option<ProjectStatus>,
        page_size: i32,
        page_token: &str,
    ) -> Result<ListProjectsResponse, Status> {
        self.db
            .list_projects(Request::new(ListProjectsRequest {
                scope: self.scope(ROOT, U1),
                under_path: under.into(),
                status: status.map(|s| s as i32),
                page_size,
                page_token: page_token.into(),
            }))
            .await
            .map(|r| r.into_inner())
    }

    /// `RenameProject`, which this release REFUSES — see `src/write.rs`.
    pub async fn rename(
        &self,
        from: &str,
        to: &str,
        expect_version: u64,
    ) -> Result<RenameProjectResponse, Status> {
        self.db
            .rename_project(Request::new(RenameProjectRequest {
                idempotency: None,
                scope: self.scope(from, U1),
                from_path: from.into(),
                to_path: to.into(),
                expect_version,
            }))
            .await
            .map(|r| r.into_inner())
    }

    /// `RenameProject` carrying an idempotency key, for the test that proves the
    /// refusal happens before the key is claimed.
    pub async fn rename_with(
        &self,
        from: &str,
        to: &str,
        expect_version: u64,
        key: Option<&str>,
    ) -> Result<RenameProjectResponse, Status> {
        self.db
            .rename_project(Request::new(RenameProjectRequest {
                idempotency: key.map(|k| Idempotency { key: k.into() }),
                scope: self.scope(from, U1),
                from_path: from.into(),
                to_path: to.into(),
                expect_version,
            }))
            .await
            .map(|r| r.into_inner())
    }

    /// `ArchiveProject`, which this release REFUSES — see `src/write.rs`.
    pub async fn archive(
        &self,
        path: &str,
        expect_version: u64,
    ) -> Result<ArchiveProjectResponse, Status> {
        self.archive_with(path, expect_version, None).await
    }

    pub async fn archive_with(
        &self,
        path: &str,
        expect_version: u64,
        key: Option<&str>,
    ) -> Result<ArchiveProjectResponse, Status> {
        self.db
            .archive_project(Request::new(ArchiveProjectRequest {
                idempotency: key.map(|k| Idempotency { key: k.into() }),
                scope: self.scope(path, U1),
                path: path.into(),
                expect_version,
            }))
            .await
            .map(|r| r.into_inner())
    }

    /// A `project_write` claim written straight into the ledger.
    ///
    /// The ledger OUTLIVES a release — a row recorded by one version of this
    /// service is read by the next — so a claim naming an rpc this release does
    /// not serve is a state a real store holds, not a contrivance. It is the
    /// only way to reach `idem::replay`'s "already used for a different
    /// operation" branch while `RegisterProject` is the one verb that claims.
    pub async fn seed_write_claim(&self, project: &str, user: &str, key: &str, rpc: &str) {
        sqlx::query(
            "INSERT INTO project_write
               (project_id, user_id, idem_key, rpc, response, request_fingerprint)
             VALUES (?, ?, ?, ?, '', ?)",
        )
        .bind(project)
        .bind(user)
        .bind(key)
        .bind(rpc)
        .bind([0u8; 32].as_slice())
        .execute(&self.pool)
        .await
        .expect("seed claim");
    }

    /// A registration written straight into `project`, bypassing the rpc.
    ///
    /// **THE STATE THIS PRESENTS IS ONE A LIVE STORE MAY ALREADY HOLD, not a
    /// contrivance.** `RegisterProject` refuses the reserved segment
    /// (`src/path.rs`), and the build that shipped before it did not — so a row
    /// at `local` is exactly what an existing database can contain, and a
    /// code-only guard does not delete one. The read paths have to be closed
    /// against that store, and writing the row is the only way to show it to
    /// them.
    ///
    /// The id is DERIVED from the path rather than minted as a UUIDv7. All a
    /// fixture needs is that two seeded rows never collide, and `path` is
    /// UNIQUE, so a function of it is unique too — while a derived id also makes
    /// a failing assertion name the row it is about instead of a random urn. It
    /// is deliberately NOT the shape `register` produces: nothing should be able
    /// to mistake a seeded row for one the service minted. The width is asserted
    /// rather than truncated, because a fixture that silently loses half an id
    /// is a test asserting against a row nobody meant to write.
    pub async fn seed_project(&self, path: &str, created_by: &str) {
        let id = format!("yadgar:project:seeded:{path}");
        assert!(
            id.len() <= 96,
            "project.id is VARCHAR(96) and this fixture id is {} characters: {id}",
            id.len()
        );
        let (source_repo, owner_user_id, visibility) = class_of(path, created_by);
        sqlx::query(
            "INSERT INTO project
               (id, version, path, display_name, status, source_repo, owner_user_id, visibility,
                created_by, updated_by)
             VALUES (?, 1, ?, '', ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(path)
        .bind(ProjectStatus::Active as i8)
        .bind(source_repo)
        .bind(owner_user_id)
        .bind(visibility)
        .bind(created_by)
        .bind(created_by)
        .execute(&self.pool)
        .await
        .expect("seed project");
    }

    /// Write a project row with the class columns stated EXPLICITLY, and answer
    /// with what the engine said.
    ///
    /// **THIS IS THE VECTOR FOR THE CHECK TESTS, and it exists because driving
    /// `RegisterProject` cannot prove them.** `write::register` derives the class
    /// columns from the path and so can never construct a disagreeing row; a test
    /// built on it would stay green with both CHECKs dropped, because the Rust
    /// code refuses first and the assertion cannot tell which layer did. This
    /// writes straight to the table on the pool, so the only thing left to refuse
    /// is the engine.
    ///
    /// It returns the `sqlx::Error` rather than a `Status` deliberately: the
    /// CONSTRAINT NAME is what the caller asserts on, and `sql::internal` maps
    /// every database error to one `Status::internal` whose message names nothing.
    pub async fn insert_class_row(
        &self,
        path: &str,
        source_repo: Option<&str>,
        owner_user_id: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let id = format!("yadgar:project:class:{path}");
        assert!(
            id.len() <= 96,
            "project.id is VARCHAR(96) and this fixture id is {} characters: {id}",
            id.len()
        );
        sqlx::query(
            "INSERT INTO project
               (id, version, path, display_name, status, source_repo, owner_user_id, visibility,
                created_by, updated_by)
             VALUES (?, 1, ?, '', ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(path)
        .bind(ProjectStatus::Active as i8)
        .bind(source_repo)
        .bind(owner_user_id)
        // STATED, never defaulted. The column is NOT NULL with no DEFAULT, and
        // this fixture is not what any visibility assertion is about.
        .bind(Visibility::Org as i8)
        .bind(U1)
        .bind(U1)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Give a project a former path, written straight into the table.
    ///
    /// **THERE IS NO RPC FOR THIS IN THIS RELEASE, ON PURPOSE.** `RenameProject`
    /// is held back until records can be retagged (`src/write.rs`), so the only
    /// way to present a store that HOLDS an alias is to write one — exactly as
    /// `task-db`'s fixture writes a visibility no RPC sets, and for the same
    /// reason. Reaching around the service for a fixture is honest; reaching
    /// around it in the code under test would not be.
    ///
    /// The read paths this exercises are not speculative: `Project.aliases`,
    /// `ResolveProject.via_alias` and "former paths that still resolve here" are
    /// contract obligations today, and a repeated field no read path fills
    /// decodes as empty with nothing reporting it.
    pub async fn seed_alias(&self, alias_path: &str, project_id: &str) {
        sqlx::query("INSERT INTO project_alias (alias_path, project_id) VALUES (?, ?)")
            .bind(alias_path)
            .bind(project_id)
            .execute(&self.pool)
            .await
            .expect("seed alias");
    }

    /// Put a project into a status no rpc can set in this release.
    ///
    /// `ArchiveProject` is held back, and `ListProjects`'s status filter and
    /// `ResolveProject`'s reported status are contract obligations regardless —
    /// so the state is seeded rather than left untested.
    pub async fn seed_status(&self, path: &str, status: ProjectStatus) {
        sqlx::query("UPDATE project SET status = ? WHERE path = ?")
            .bind(status as i8)
            .bind(path)
            .execute(&self.pool)
            .await
            .expect("seed status");
    }

    pub async fn touch(&self, paths: &[&str], seconds: i64) -> Result<(), Status> {
        self.db
            .touch_projects(Request::new(TouchProjectsRequest {
                scope: self.scope(ROOT, U1),
                paths: paths.iter().map(|p| (*p).to_string()).collect(),
                seen_at: Some(prost_types::Timestamp { seconds, nanos: 0 }),
            }))
            .await
            .map(|_| ())
    }

    /// What is actually in the column — the only way to tell "assigned" from
    /// "took the caller's word and happened to agree".
    pub async fn stored_status(&self, path: &str) -> i8 {
        self.column(path, "status").await
    }

    pub async fn stored_version(&self, path: &str) -> u64 {
        self.column(path, "version").await
    }

    /// `(source_repo, owner_user_id)` as the row holds them.
    ///
    /// Read as a PAIR rather than one at a time, because the fact under test is
    /// their exclusivity: asserting them separately lets a row with both set
    /// satisfy two assertions that each look correct.
    pub async fn stored_class(&self, path: &str) -> (Option<String>, Option<String>) {
        sqlx::query_as("SELECT source_repo, owner_user_id FROM project WHERE path = ?")
            .bind(path)
            .fetch_one(&self.pool)
            .await
            .expect("read the class columns")
    }

    pub async fn stored_visibility(&self, path: &str) -> i8 {
        self.column(path, "visibility").await
    }

    /// The `updated_at` of a registration, as an epoch second.
    ///
    /// Read through the same `UNIX_TIMESTAMP` cast the service uses, because
    /// `sqlx` here decodes no `TIMESTAMP` at all — see `src/rows.rs`.
    pub async fn stored_updated_at(&self, path: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT CAST(UNIX_TIMESTAMP(updated_at) AS SIGNED) FROM project \
                            WHERE path = ?",
        )
        .bind(path)
        .fetch_one(&self.pool)
        .await
        .expect("updated_at")
    }

    /// Push a registration's `updated_at` back to a known moment.
    ///
    /// **WITHOUT THIS, THE ASSERTION IT SERVES CANNOT FAIL, AND THAT WAS
    /// MEASURED RATHER THAN REASONED.** With `ON UPDATE CURRENT_TIMESTAMP` added
    /// back to migration 1 — the exact defect `tests/touch.rs` claims to catch —
    /// the test still passed: a `TIMESTAMP` stores whole seconds, and the
    /// registration and the flush land inside the same one, so "unchanged" and
    /// "reset to now" are the same number. Writing a moment in the past makes the
    /// two distinguishable without making the test sleep.
    ///
    /// An explicit assignment in an `UPDATE` wins over an `ON UPDATE` clause, so
    /// this statement sets what it says even under the mutant.
    pub async fn backdate_updated_at(&self, path: &str, seconds: i64) {
        sqlx::query("UPDATE project SET updated_at = FROM_UNIXTIME(?) WHERE path = ?")
            .bind(seconds)
            .bind(path)
            .execute(&self.pool)
            .await
            .expect("backdate");
    }

    pub async fn stored_last_seen_at(&self, path: &str) -> Option<i64> {
        sqlx::query_scalar(
            "SELECT CAST(UNIX_TIMESTAMP(last_seen_at) AS SIGNED) FROM project \
                            WHERE path = ?",
        )
        .bind(path)
        .fetch_one(&self.pool)
        .await
        .expect("last_seen_at")
    }

    async fn column<T>(&self, path: &str, column: &'static str) -> T
    where
        T: for<'r> sqlx::Decode<'r, sqlx::MySql> + sqlx::Type<sqlx::MySql> + Send + Unpin,
    {
        // AUDIT: `column` is a literal in this file; `path` is bound.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {column} FROM project WHERE path = ?"
        )))
        .bind(path)
        .fetch_one(&self.pool)
        .await
        .expect("select")
        .try_get(column)
        .expect("column")
    }

    /// The bytes actually in `project_write.request_fingerprint` for one claim.
    ///
    /// Read from the COLUMN rather than recomputed, because the storage encoding
    /// is part of what a known-answer test pins: a digest of some other width
    /// silently padded out by `BINARY(32)` is invisible to an assertion that
    /// stops at the function's return value.
    pub async fn stored_fingerprint(&self, project: &str, user: &str, key: &str) -> Vec<u8> {
        sqlx::query_scalar(
            "SELECT request_fingerprint FROM project_write
              WHERE project_id = ? AND user_id = ? AND idem_key = ?",
        )
        .bind(project)
        .bind(user)
        .bind(key)
        .fetch_one(&self.pool)
        .await
        .expect("select request_fingerprint")
    }

    /// How many rows the table holds at *path* — 0 or 1, because `path` is
    /// UNIQUE.
    ///
    /// **A REFUSAL IS ONLY PROVED BY THE ABSENCE OF THE ROW.** A guard that
    /// inserted and then reported an error would satisfy an `expect_err`, and the
    /// store would hold exactly the row the guard exists to keep out.
    /// `stored_class` cannot be used for this: it `fetch_one`s and panics on an
    /// absent row, which is the outcome under test.
    pub async fn row_count(&self, path: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM project WHERE path = ?")
            .bind(path)
            .fetch_one(&self.pool)
            .await
            .expect("count")
    }

    pub async fn alias_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM project_alias")
            .fetch_one(&self.pool)
            .await
            .expect("count")
    }
}

/// The class columns a fixture row carries. The CLASS from its path; the org
/// class's repository from [`FIXTURE_REPO`].
///
/// **THE CLASS RULE IS STATED HERE RATHER THAN IMPORTED so the fixture cannot
/// borrow a defect from the code under test**, and it is one line of the plan: the
/// class is the path, and a private path is `local/<account>/<path>` by
/// construction of the id (ADR-0605). What the fixture must not do is decide a
/// class the CHECKs would refuse — a fixture that tripped a constraint in every
/// unrelated test would say nothing about either.
///
/// **THE REPOSITORY IS A SENTINEL AND NOT THE PATH.** It used to be the path,
/// mirroring what `write::register` derived; `write::register` no longer derives
/// it at all, and a seeded row has no request to take it from. The sentinel is the
/// stronger choice in any case — see [`FIXTURE_REPO`] — because a seeded row whose
/// repository equals its own path cannot distinguish a resolver returning the
/// column from one returning `resolved_path`.
///
/// The BARE reserved segment is org-class here, and that is not an oversight:
/// `local` does not match `local/%`, so the engine's own rule puts it on the org
/// side, and `tests/registry.rs` seeds exactly that row to present a store
/// holding one.
fn class_of(path: &str, actor: &str) -> (Option<String>, Option<String>, i8) {
    if is_private_path(path) {
        (None, Some(actor.to_string()), Visibility::Private as i8)
    } else {
        (Some(FIXTURE_REPO.to_string()), None, Visibility::Org as i8)
    }
}

/// Is *path* in the PRIVATE class? The engine's `LIKE 'local/%'` under a
/// case-folding collation, and `crate::path::is_private_class`, spelled a third
/// time — deliberately, for the reason [`class_of`] gives.
fn is_private_path(path: &str) -> bool {
    path.split_once('/')
        .is_some_and(|(root, _)| root.eq_ignore_ascii_case("local"))
}

// ─────────────────────────────────────────────────────────────────────────
// THE EXIT CHAIN HARNESS (ledger 748), copied in shape from `yadgarhq/task`'s
// canary (`task#70`) and adapted for a binary that actually dials an engine
// before it listens. `World` above never runs the BINARY — it calls
// `ProjectDb` directly over a pool this test process opened — so it cannot
// see `main`'s own wiring of `boot::shutdown` and `rotate::watch` into the
// `select!` that ends `serve`. Only running the binary can.
//
// WHY A MOUNT NAMESPACE. `yadgar_lifecycle::rotate::Configuration::mounted`
// reads `/etc/yadgar/config/shared/shared.yaml` unconditionally, with no
// environment override — the path IS the chart's `mountPath`. A test cannot
// write under the host's `/etc`, so every run goes through `unshare -rm`,
// which gives the binary a private mount table in which `/etc` is an overlay
// over the real one, with the document's directory bind-mounted into it —
// the same shape kubelet gives a ConfigMap volume, and the only one that
// lets a test rewrite the file from OUTSIDE the namespace afterwards.
//
// NO PROBE, NO SKIP. If the runner forbids unprivileged user namespaces,
// `mount` fails, the script exits non-zero before the binary starts, and
// every wait below reports that with the captured stderr.

use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// The binary under test.
pub const BIN: &str = env!("CARGO_BIN_EXE_yadgar-project-db");

/// The rotation schedule every run is given: poll each second, no splay. A
/// rewrite is therefore noticed within one second and acted on at once.
pub const FIXTURE: &str = "tlsRotation:\n  pollSeconds: 1\n  splayMaxSeconds: 0\n";

/// The poll and splay in [`FIXTURE`], for deadlines derived from them.
pub const POLL: Duration = Duration::from_secs(1);
pub const SPLAY_MAX: Duration = Duration::from_secs(0);

/// What every deadline adds on top of the time the binary is allowed.
/// Generous, because a shared CI runner is slow, a probe and a migration are
/// both real round trips to the engine, and the waits it bounds are seconds
/// long regardless.
pub const MARGIN: Duration = Duration::from_secs(10);

/// How long a boot may take to reach its "listening" line. Longer than
/// `task`'s: this binary probes AND migrates a real engine before it opens a
/// socket, neither of which a passthrough service does.
pub const BOOT_DEADLINE: Duration = Duration::from_secs(30);

/// The script `unshare` runs. Paths arrive as positional arguments — `$1` the
/// per-test root, `$2` the binary — rather than interpolated into the text.
const SCRIPT: &str = r#"set -e
mount -t overlay overlay -o "lowerdir=/etc,upperdir=$1/upper,workdir=$1/work" /etc
mkdir -p /etc/yadgar/config/shared
mount --bind "$1/shared" /etc/yadgar/config/shared
exec "$2""#;

/// `user, password, host, port` out of `mysql://user:password@host:port[/db]`
/// — [`dsn`]'s own format, stated here rather than imported because nothing
/// in this crate parses a DSN. PURE.
fn parse_dsn(dsn: &str) -> (String, String, String, u16) {
    let rest = dsn
        .strip_prefix("mysql://")
        .expect("YADGAR_TEST_DSN must start with mysql://");
    let (creds, host_port) = rest
        .split_once('@')
        .expect("YADGAR_TEST_DSN must carry user:password@host:port");
    let (user, password) = creds.split_once(':').unwrap_or((creds, ""));
    let host_port = host_port.split('/').next().unwrap_or(host_port);
    let (host, port) = host_port
        .split_once(':')
        .expect("YADGAR_TEST_DSN must carry host:port");
    (
        user.to_string(),
        password.to_string(),
        host.to_string(),
        port.parse()
            .expect("YADGAR_TEST_DSN's port must be a number"),
    )
}

/// A throwaway database this process creates and the SPAWNED BINARY migrates
/// into — `World::fresh`'s own DDL, without opening a pool here: the binary
/// owns that pool, not this test.
pub async fn fresh_boot_database(name: &str) {
    let mut root = sqlx::MySqlConnection::connect(&dsn())
        .await
        .expect("connect to create the boot fixture's database");
    for stmt in [
        format!("DROP DATABASE IF EXISTS {name}"),
        format!("CREATE DATABASE {name}"),
    ] {
        // AUDIT: `name` is a literal at every call site in this test target.
        sqlx::raw_sql(sqlx::AssertSqlSafe(stmt))
            .execute(&mut root)
            .await
            .expect("ddl");
    }
}

/// The environment the chart's `deployment.yaml` renders for a CLEARTEXT
/// deployment against a real engine, with loopback listeners.
///
/// Port 0 on both listeners, because the cases in a target run in parallel.
/// `DB_HOST`/`DB_PORT`/`DB_USER` and the password come straight out of
/// [`YADGAR_TEST_DSN`][dsn] — the same engine every other integration test in
/// this crate already migrates against — so this harness introduces no
/// second source for "which database is real". `password_file` is written by
/// the caller (`Booted::start`'s caller, in practice) and handed back here
/// only as a path: `CredentialSource::SecretFile` reads it directly and has
/// no mount requirement of its own, unlike the rotation document.
pub fn boot_cleartext_env(db_name: &str, password_file: &Path) -> Vec<(String, String)> {
    let (user, password, host, port) = parse_dsn(&dsn());
    std::fs::write(password_file, &password).expect("the boot fixture's password file");
    vec![
        ("DB_HOST".to_string(), host),
        ("DB_PORT".to_string(), port.to_string()),
        ("DB_NAME".to_string(), db_name.to_string()),
        ("DB_USER".to_string(), user),
        (
            "DB_PASSWORD_FILE".to_string(),
            password_file.display().to_string(),
        ),
        // SMALL ON PURPOSE: this harness opens one pool against one throwaway
        // database, never the sizes a chart renders for a real deployment.
        ("DB_MAX_CONNECTIONS".to_string(), "4".to_string()),
        ("REPLICAS".to_string(), "1".to_string()),
        ("DB_ENGINE_MAX_CONNECTIONS".to_string(), "151".to_string()),
        // The fixture engine speaks no TLS; `disabled` is `parse_ssl_mode`'s
        // own word for that, distinct from the listener's `LISTEN_TLS_ENABLED`
        // below.
        ("DB_SSL_MODE".to_string(), "disabled".to_string()),
        (
            "DB_MIGRATION_LOCK_TIMEOUT_SECONDS".to_string(),
            "30".to_string(),
        ),
        ("LISTEN".to_string(), "127.0.0.1:0".to_string()),
        ("METRICS_LISTEN".to_string(), "127.0.0.1:0".to_string()),
        ("LISTEN_TLS_ENABLED".to_string(), "0".to_string()),
    ]
}

/// One line the binary wrote, and which stream it came from.
enum Line {
    Out(String),
    Err(String),
}

/// The binary, running in its own mount namespace, with its output pumped.
pub struct Booted {
    child: Child,
    lines: Receiver<Line>,
    seen: Vec<String>,
    root: PathBuf,
}

impl Booted {
    /// Start the binary with exactly `vars` (and `PATH`) in its environment.
    pub fn start(vars: &[(String, String)]) -> Self {
        let root = fresh_root();
        // `unshare`, `mount` and `sh` are found on the caller's PATH; on some
        // hosts none of them is under /usr/bin. A rig with no PATH fails here
        // rather than guessing one.
        let path = std::env::var_os("PATH").expect("the test runner must have a PATH");
        let mut child = Command::new("unshare")
            .args(["-rm", "sh", "-c", SCRIPT, "sh"])
            .arg(&root)
            .arg(BIN)
            .env_clear()
            .env("PATH", path)
            .envs(vars.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("`unshare` could not be started: {e}"));

        // ONE THREAD PER STREAM, pumping for the life of the child. Reading
        // only until the "listening" line would leave the pipe to fill and
        // the binary to block on its next log line.
        let (tx, lines) = mpsc::channel();
        let out = child.stdout.take().expect("stdout was piped");
        let err = child.stderr.take().expect("stderr was piped");
        let tx_err = tx.clone();
        std::thread::spawn(move || pump(out, move |l| tx.send(Line::Out(l)).is_ok()));
        std::thread::spawn(move || pump(err, move |l| tx_err.send(Line::Err(l)).is_ok()));

        Self {
            child,
            lines,
            seen: Vec::new(),
            root,
        }
    }

    /// The binary's pid. `exec` all the way down makes it the direct child.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait for a line satisfying `matches`, or fail on `deadline`.
    ///
    /// A child whose output ends first — it exited, or the mount namespace
    /// was refused and it never started — fails at once with what it
    /// printed, rather than as a timeout.
    pub fn wait_for_line(
        &mut self,
        what: &str,
        deadline: Duration,
        matches: impl Fn(&str) -> bool,
    ) -> String {
        let until = Instant::now() + deadline;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(Line::Out(l) | Line::Err(l)) => {
                    self.seen.push(l.clone());
                    if matches(&l) {
                        return l;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.fail(&format!("waited {deadline:?} for {what}; it never came"))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.reap(Duration::from_secs(5));
                    self.fail(&format!(
                        "the binary's output ended before {what} ({})",
                        describe(status)
                    ))
                }
            }
        }
    }

    /// Wait for the boot to reach the line naming "project-db listening",
    /// logged once the signal handlers are armed and the server is spawned
    /// (`src/main.rs::serve`).
    pub fn wait_until_listening(&mut self) {
        self.wait_for_line("the \"project-db listening\" line", BOOT_DEADLINE, |l| {
            l.contains("project-db listening")
        });
    }

    /// Send SIGTERM, the way kubelet ends a pod.
    pub fn terminate(&mut self) {
        let status = Command::new("kill")
            .args(["-TERM", &self.pid().to_string()])
            .status()
            .unwrap_or_else(|e| panic!("`kill` could not be started: {e}"));
        if !status.success() {
            self.fail(&format!("`kill -TERM` failed: {status}"));
        }
    }

    /// Wait for the process to exit, or fail on `deadline`.
    pub fn wait_for_exit(&mut self, what: &str, deadline: Duration) -> ExitStatus {
        match self.reap(deadline) {
            Some(status) => {
                self.drain_lines();
                status
            }
            None => self.fail(&format!(
                "waited {deadline:?} for the process to exit {what}; it was still running"
            )),
        }
    }

    /// Replace the mounted `shared.yaml` the way kubelet replaces a projected
    /// file: write a sibling, then rename it over the original.
    pub fn rewrite_shared(&self, contents: &str) {
        let dir = self.root.join("shared");
        let staged = dir.join(".shared.yaml.next");
        std::fs::write(&staged, contents).expect("the staged document must be written");
        std::fs::rename(&staged, dir.join("shared.yaml"))
            .expect("the staged document must replace the mounted one");
    }

    /// Every line the binary has written so far.
    pub fn seen(&self) -> &[String] {
        &self.seen
    }

    /// Fail the test: kill the child, then panic with everything it printed.
    pub fn fail(&mut self, why: &str) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.drain_lines();
        panic!(
            "{why}\n--- everything the binary wrote ---\n{}",
            self.seen.join("\n")
        );
    }

    fn reap(&mut self, deadline: Duration) -> Option<ExitStatus> {
        let until = Instant::now() + deadline;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() >= until => return None,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => panic!("the child could not be waited on: {e}"),
            }
        }
    }

    fn drain_lines(&mut self) {
        // The pumps end at EOF, which follows the exit closely; a short
        // bound keeps a stray grandchild holding the pipe from hanging the
        // test.
        while let Ok(Line::Out(l) | Line::Err(l)) =
            self.lines.recv_timeout(Duration::from_millis(500))
        {
            self.seen.push(l);
        }
    }
}

impl Drop for Booted {
    fn drop(&mut self) {
        // A panicking test must not leave a server running.
        let _ = self.child.kill();
        let _ = self.child.wait();
        remove_root(&self.root);
    }
}

/// The exit status as a sentence that says whether a signal ended it.
pub fn describe(status: Option<ExitStatus>) -> String {
    match status {
        None => "still running".to_string(),
        Some(s) => format!("code {:?}, signal {:?}", s.code(), s.signal()),
    }
}

/// A per-test directory: the overlay's upper and work dirs, and the
/// directory bind-mounted as `/etc/yadgar/config/shared`.
///
/// Under the system temp dir, which must not itself be an overlay: an
/// overlay's upperdir cannot sit on one. The name carries a counter as well
/// as the pid, because the cases of a target run on threads of one process
/// (ledger 706).
fn fresh_root() -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "yadgar-project-db-exit-chain-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    for dir in ["upper", "work", "shared"] {
        std::fs::create_dir_all(root.join(dir)).expect("the test root must be created");
    }
    std::fs::write(root.join("shared").join("shared.yaml"), FIXTURE)
        .expect("the fixture document must be written");
    root
}

/// Best effort. Overlayfs leaves `work/work` with mode 0, so it is opened up
/// before the tree is removed.
fn remove_root(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(
        root.join("work").join("work"),
        std::fs::Permissions::from_mode(0o700),
    );
    let _ = std::fs::remove_dir_all(root);
}

fn pump(stream: impl Read, mut send: impl FnMut(String) -> bool) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        if !send(line) {
            return;
        }
    }
}
