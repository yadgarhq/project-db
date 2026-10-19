//! A store an OLDER release migrated, meeting this release's image.
//!
//! **EVERY OTHER SUITE HERE STARTS FROM AN EMPTY DATABASE, AND THAT IS HOW
//! v0.7.0 SHIPPED BROKEN.** `World::fresh` applies this release's migrations to
//! nothing, so it can only ever see the schema a NEW install gets. #33 (v0.2.0)
//! edited migration 1 in place to add the class columns; a store that applied
//! v0.1.x's migration 1 records version 1 in its ledger, never receives them, and
//! answers `SyncProjects`, `ResolveProject` and `RegisterProject` with
//! `1054 Unknown column 'source_repo'`. kind-yadgar was exactly that store on
//! 2026-10-18: "schema at migration 5", zero rows, every seeder sync INTERNAL.
//!
//! [`V0_1`] is v0.1.0 through v0.1.18's migration set, frozen VERBATIM (the SQL
//! is identical across every one of those tags). It is a literal, never read
//! from git at run time: it is history, and history does not change.

mod support;
use support::*;

use yadgar_project_db::schema;
use yadgar_store::migrate::Migration;

/// v0.1.0–v0.1.18's migrations 1–4, SQL byte-for-byte as those tags carry it.
#[allow(non_snake_case)]
fn V0_1() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            name: "create_project".into(),
            sql: "CREATE TABLE project (
                  id            VARCHAR(96)     NOT NULL PRIMARY KEY,
                  version       BIGINT UNSIGNED NOT NULL DEFAULT 1,
                  path          VARCHAR(255)    NOT NULL,
                  display_name  VARCHAR(255)    NOT NULL,
                  status        TINYINT         NOT NULL,
                  created_by    VARCHAR(64)     NOT NULL,
                  updated_by    VARCHAR(64)     NOT NULL,
                  created_at    TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  updated_at    TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  last_seen_at  TIMESTAMP       NULL     DEFAULT NULL,
                  UNIQUE KEY uq_project_path (path),
                  KEY ix_project_status (status)
              ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
                .into(),
        },
        Migration {
            version: 2,
            name: "project_alias".into(),
            sql: "CREATE TABLE project_alias (
                  alias_path VARCHAR(255) NOT NULL PRIMARY KEY,
                  project_id VARCHAR(96)  NOT NULL,
                  created_at TIMESTAMP    NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  KEY ix_project_alias_project (project_id)
              ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
                .into(),
        },
        Migration {
            version: 3,
            name: "project_write_idempotency".into(),
            sql: "CREATE TABLE project_write (
                  project_id          VARCHAR(255)    NOT NULL,
                  user_id             VARCHAR(64)     NOT NULL,
                  idem_key            VARCHAR(255)    NOT NULL,
                  rpc                 VARCHAR(32)     NOT NULL,
                  response            VARBINARY(4096) NOT NULL,
                  request_fingerprint BINARY(32)      NOT NULL,
                  created_at          TIMESTAMP       NOT NULL DEFAULT CURRENT_TIMESTAMP,
                  PRIMARY KEY (project_id, user_id, idem_key)
              ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
                .into(),
        },
        Migration {
            version: 4,
            name: "pin_the_path_collation".into(),
            sql: "ALTER TABLE project
                MODIFY path VARCHAR(255)
                  CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci NOT NULL;
              ALTER TABLE project_alias
                MODIFY alias_path VARCHAR(255)
                  CHARACTER SET utf8mb4 COLLATE utf8mb4_general_ci NOT NULL"
                .into(),
        },
    ]
}

const R: &str = "tapir-0e61";
const P1: &str = "tapir-0e61/one";
const P2: &str = "tapir-0e61/two";
const REPO: &str = "tapir-0e61/registry";

/// THE INCIDENT, end to end: the seeder's sync, then the read the gateway's
/// remediation depends on, then a private registration — every writer and
/// reader of the class columns, on a store v0.1.x created.
#[tokio::test]
async fn a_store_v0_1_migrated_syncs_resolves_and_registers_after_the_upgrade() {
    let w = World::upgraded("project_db_upgrade_e2e", V0_1()).await;

    let r = w
        .sync(
            &[declared(R, "tapir", REPO)],
            &[
                declared(P1, "one", "tapir-0e61/one-repo"),
                declared(P2, "two", "tapir-0e61/two-repo"),
            ],
            false,
        )
        .await
        .expect("SyncProjects on an upgraded store");
    assert_eq!((r.inserted, r.updated, r.archived), (3, 0, 0));

    let resolved = w.resolve(P1).await.expect("ResolveProject");
    assert_eq!(resolved.source_repo, "tapir-0e61/one-repo");

    let private = format!("local/{U1_SEG}/scratch");
    w.try_register(&private, "scratch")
        .await
        .expect("RegisterProject, private class");
    assert_eq!(w.stored_class(&private).await, (None, Some(U1.to_string())));
}

/// Every column, CHECK and index a fresh install has, an upgraded store has —
/// with the same type, nullability, default, collation and POSITION. This is
/// the sibling catcher: a query is only right against the schema it meets.
#[tokio::test]
async fn an_upgraded_store_has_exactly_the_schema_a_fresh_one_has() {
    let fresh = World::fresh("project_db_upgrade_parity_fresh").await;
    let upgraded = World::upgraded("project_db_upgrade_parity_old", V0_1()).await;
    assert_eq!(
        shape(&upgraded.pool, "project_db_upgrade_parity_old").await,
        shape(&fresh.pool, "project_db_upgrade_parity_fresh").await
    );
}

/// DDL is not transactional, so a failure inside a migration re-runs the whole
/// of it on the next boot — and on a FRESH install every clause of migration 6
/// meets columns and constraints migration 1 already made. Both must be no-ops.
#[tokio::test]
async fn the_class_column_migration_is_a_no_op_wherever_it_has_already_landed() {
    for (db, history) in [
        ("project_db_upgrade_rerun_fresh", Vec::new()),
        ("project_db_upgrade_rerun_old", V0_1()),
    ] {
        let w = World::upgraded(db, history).await;
        let before = shape(&w.pool, db).await;
        let again: Vec<Migration> = schema::migrations()
            .expect("set")
            .pending_after(5)
            .map(|m| Migration {
                // A version the ledger has not seen, so `apply` runs the SQL.
                version: m.version + 100,
                name: m.name.clone(),
                sql: m.sql.clone(),
            })
            .collect();
        assert!(!again.is_empty(), "migration 6 exists");
        let set = yadgar_store::migrate::MigrationSet::new(again).expect("set");
        World::apply(&w.pool, &set).await.expect("re-run");
        assert_eq!(shape(&w.pool, db).await, before, "{db}");
    }
}

/// A v0.1.x row has no class: no repository and no owner. Nothing can derive
/// either, so the upgrade REFUSES rather than storing a row in no class — and
/// it refuses naming the constraint, so the operator knows which rows to settle.
#[tokio::test]
async fn an_upgrade_over_v0_1_rows_refuses_naming_the_class_constraint() {
    let db = "project_db_upgrade_populated";
    let pool = World::history_only(db, V0_1()).await;
    sqlx::query(
        "INSERT INTO project (id, path, display_name, status, created_by, updated_by)
         VALUES ('urn:x', 'tapir-0e61/legacy', 'legacy', 1, 'u1', 'u1')",
    )
    .execute(&pool)
    .await
    .expect("a v0.1.x row");

    let err = World::apply(&pool, &schema::migrations().expect("set"))
        .await
        .expect_err("an unclassifiable row must stop the upgrade");
    assert!(
        err.to_string().contains("ck_project_class"),
        "names the constraint: {err}"
    );
}

/// `(table, column, type, nullable, default, collation, position)`.
type Column = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    u64,
);

type Shape = (
    Vec<Column>,
    Vec<(String, String, String)>,
    Vec<(String, String, String, u64, i64)>,
);

/// `information_schema`'s view of one database: columns, CHECKs, indexes.
async fn shape(pool: &sqlx::MySqlPool, db: &str) -> Shape {
    let columns = sqlx::query_as(
        "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_DEFAULT,
                COLLATION_NAME, CAST(ORDINAL_POSITION AS UNSIGNED)
           FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = ?
          ORDER BY TABLE_NAME, ORDINAL_POSITION",
    )
    .bind(db)
    .fetch_all(pool)
    .await
    .expect("columns");
    let checks = sqlx::query_as(
        "SELECT TABLE_NAME, CONSTRAINT_NAME, CHECK_CLAUSE
           FROM information_schema.CHECK_CONSTRAINTS WHERE CONSTRAINT_SCHEMA = ?
          ORDER BY TABLE_NAME, CONSTRAINT_NAME",
    )
    .bind(db)
    .fetch_all(pool)
    .await
    .expect("checks");
    let indexes = sqlx::query_as(
        "SELECT TABLE_NAME, INDEX_NAME, COLUMN_NAME,
                CAST(SEQ_IN_INDEX AS UNSIGNED), CAST(NON_UNIQUE AS SIGNED)
           FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ?
          ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
    )
    .bind(db)
    .fetch_all(pool)
    .await
    .expect("indexes");
    (columns, checks, indexes)
}
