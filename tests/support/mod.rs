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
use yadgar_project_db::pb::yadgar::common::v1::{Idempotency, Scope};
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

/// The pool every fixture opens.
///
/// **A fixture that says nothing runs at sqlx's default of ten, which is a size
/// production never ships.** `DB_MAX_CONNECTIONS` defaults to eight and D80 made
/// it operator-settable downwards, so the sizes that matter are small ones. Four
/// is above `store`'s `MIN_CONNECTIONS` of two, so migration keeps the two
/// connections it holds at once.
const DEFAULT_POOL_SIZE: u32 = 4;

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
            // STATED, never inherited. See [`DEFAULT_POOL_SIZE`].
            .max_connections(DEFAULT_POOL_SIZE)
            .connect(&format!("{}/{name}", base()))
            .await
            .expect("pool");
        yadgar_store::migrate::apply(&pool, &schema::migrations().expect("set"))
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

    pub async fn register_as(
        &self,
        path: &str,
        display_name: &str,
        user: &str,
        key: Option<&str>,
    ) -> Result<RegisterProjectResponse, Status> {
        self.db
            .register_project(Request::new(RegisterProjectRequest {
                idempotency: key.map(|k| Idempotency { key: k.into() }),
                scope: self.scope(path, user),
                path: path.into(),
                display_name: display_name.into(),
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
        sqlx::query(
            "INSERT INTO project
               (id, version, path, display_name, status, created_by, updated_by)
             VALUES (?, 1, ?, '', ?, ?, ?)",
        )
        .bind(&id)
        .bind(path)
        .bind(ProjectStatus::Active as i8)
        .bind(created_by)
        .bind(created_by)
        .execute(&self.pool)
        .await
        .expect("seed project");
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

    pub async fn alias_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM project_alias")
            .fetch_one(&self.pool)
            .await
            .expect("count")
    }
}
