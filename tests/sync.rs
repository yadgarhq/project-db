//! `SyncProjects`: the whole declaration in one transaction (ADR-0936,
//! `plans/the-d35-seeder.md` stage 2). The numbered cases are the plan's.
//!
//! Every case runs on its own database, because a sync archives EVERY org-class
//! row it is not told about: two cases sharing a schema would archive each
//! other's rows, and trip the mass-archive guard at random.
//!
//! The caller check (plan case 10) has a half here — a refused caller never
//! takes a connection — and a half over real handshakes, in
//! `tests/sync_caller.rs`.

mod support;
use support::*;

use std::time::Duration;

use tonic::Code;
use yadgar_project_db::pb::yadgar::project::v1::{
    DeclaredProject, ProjectStatus, SyncProjectsResponse,
};
use yadgar_project_db::service::ProjectDb;

/// The sentinel root of this suite — not [`ROOT`], so no assertion here can be
/// satisfied by a row another fixture helper seeded.
const R: &str = "okapi-5d10";
const P1: &str = "okapi-5d10/one";
const P2: &str = "okapi-5d10/two";
const P3: &str = "okapi-5d10/three";
const P4: &str = "okapi-5d10/four";
const REPO: &str = "okapi-5d10/registry";

const ACTIVE: i8 = ProjectStatus::Active as i8;
const ARCHIVED: i8 = ProjectStatus::Archived as i8;

fn root() -> DeclaredProject {
    declared(R, "okapi", REPO)
}

fn project(path: &str) -> DeclaredProject {
    declared(path, &format!("name of {path}"), &format!("{path}-repo"))
}

fn three() -> Vec<DeclaredProject> {
    vec![project(P1), project(P2), project(P3)]
}

fn counts(r: &SyncProjectsResponse) -> (u32, u32, u32, u32, u32) {
    (
        r.inserted,
        r.updated,
        r.archived,
        r.reactivated,
        r.unchanged,
    )
}

// ---- 1, 2: insert, then converge -------------------------------------------

/// Plan case 1.
#[tokio::test]
async fn a_declaration_into_an_empty_store_inserts_every_row_active() {
    let w = World::fresh("project_db_sync_insert").await;
    let r = w.sync(&[root()], &three(), false).await.expect("sync");
    assert_eq!(counts(&r), (4, 0, 0, 0, 0));

    let rows = w.org_rows().await;
    assert_eq!(rows.len(), 4);
    for (path, status, version, display_name, source_repo) in &rows {
        assert_eq!(*status, ACTIVE, "{path}");
        assert_eq!(*version, 1, "{path}");
        let want = if path == R { root() } else { project(path) };
        assert_eq!(display_name, &want.display_name, "{path}");
        assert_eq!(source_repo.as_deref(), Some(want.source_repo.as_str()));
    }
    // THE PRINCIPAL IS THE SCOPE'S, on both stamps.
    let (created, updated): (String, String) =
        sqlx::query_as("SELECT created_by, updated_by FROM project WHERE path = ?")
            .bind(P1)
            .fetch_one(&w.pool)
            .await
            .unwrap();
    assert_eq!((created.as_str(), updated.as_str()), (SEEDER, SEEDER));
    assert_eq!(
        w.stored_visibility(P1).await,
        yadgar_project_db::pb::yadgar::common::v1::Visibility::Org as i8,
        "an org row is ORG-visible (D12)"
    );
}

/// Plan case 2.
#[tokio::test]
async fn the_same_declaration_twice_changes_nothing() {
    let w = World::fresh("project_db_sync_converge").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    let before = w.org_rows().await;

    let r = w.sync(&[root()], &three(), false).await.unwrap();
    assert_eq!(counts(&r), (0, 0, 0, 0, 4));
    assert_eq!(
        w.org_rows().await,
        before,
        "no column, version included, moved"
    );
}

// ---- 3: overwrite ------------------------------------------------------------

/// Plan case 3, `display_name` and `source_repo` each.
#[tokio::test]
async fn a_changed_name_or_repository_overwrites_and_bumps_the_version() {
    let w = World::fresh("project_db_sync_overwrite").await;
    w.sync(&[root()], &three(), false).await.unwrap();

    let renamed = declared(P1, "a new name", &format!("{P1}-repo"));
    let moved = declared(P2, &format!("name of {P2}"), "okapi-5d10/elsewhere");
    let r = w
        .sync(&[root()], &[renamed, moved, project(P3)], false)
        .await
        .unwrap();
    assert_eq!(counts(&r), (0, 2, 0, 0, 2));

    let rows = w.org_rows().await;
    let row = |p: &str| rows.iter().find(|r| r.0 == p).unwrap().clone();
    assert_eq!((row(P1).2, row(P1).3.as_str()), (2, "a new name"));
    assert_eq!(
        (row(P2).2, row(P2).4.as_deref()),
        (2, Some("okapi-5d10/elsewhere"))
    );
    assert_eq!(row(P3).2, 1, "an unchanged row keeps its version");
}

// ---- 4, 5: archive, reactivate ----------------------------------------------

/// Plan case 4.
#[tokio::test]
async fn a_project_dropped_from_the_declaration_is_archived_and_still_resolves() {
    let w = World::fresh("project_db_sync_archive").await;
    w.sync(&[root()], &three(), false).await.unwrap();

    let r = w
        .sync(&[root()], &[project(P1), project(P2)], false)
        .await
        .unwrap();
    assert_eq!(counts(&r), (0, 0, 1, 0, 3));

    assert_eq!(w.row_count(P3).await, 1, "archived, never deleted");
    assert_eq!(w.stored_status(P3).await, ARCHIVED);
    assert_eq!(w.stored_version(P3).await, 2);

    let resolved = w.resolve(P3).await.expect("resolve");
    assert!(resolved.exact);
    assert_eq!(resolved.status, ProjectStatus::Archived as i32);

    let active = w
        .list_page(R, Some(ProjectStatus::Active), 0, "")
        .await
        .unwrap()
        .projects;
    assert!(active.iter().all(|p| p.path != P3), "ACTIVE omits it");
}

/// Plan case 5.
#[tokio::test]
async fn a_re_declared_archived_project_is_active_again() {
    let w = World::fresh("project_db_sync_reactivate").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    w.sync(&[root()], &[project(P1), project(P2)], false)
        .await
        .unwrap();

    // RE-DECLARED WITH A NEW NAME TOO: reactivated, and overwritten, and
    // counted once — as reactivated.
    let back = declared(P3, "back again", &format!("{P3}-repo"));
    let r = w
        .sync(&[root()], &[project(P1), project(P2), back], false)
        .await
        .unwrap();
    assert_eq!(counts(&r), (0, 0, 0, 1, 3));
    assert_eq!(w.stored_status(P3).await, ACTIVE);
    assert_eq!(w.stored_version(P3).await, 3);
    assert_eq!(w.get(P3).await.unwrap().display_name, "back again");
}

// ---- 6: private rows ----------------------------------------------------------

/// Plan case 6. Registered, then put through an insert, an overwrite and an
/// archive — every column of the private row reads back identical.
#[tokio::test]
async fn a_private_row_survives_every_sync_untouched() {
    let w = World::fresh("project_db_sync_private").await;
    let private = "local/u1/okapi-notes";
    w.try_register(private, "notes").await.expect("register");
    let read = || async {
        sqlx::query_as::<_, (i8, u64, String, Option<String>, Option<String>, String)>(
            "SELECT status, version, display_name, source_repo, owner_user_id, updated_by
               FROM project WHERE path = ?",
        )
        .bind(private)
        .fetch_one(&w.pool)
        .await
        .unwrap()
    };
    let before = read().await;

    w.sync(&[root()], &three(), false).await.unwrap();
    w.sync(&[root()], &[project(P1), project(P2), project(P3)], false)
        .await
        .unwrap();
    w.sync(&[root()], &[project(P1), project(P2)], false)
        .await
        .unwrap();

    assert_eq!(read().await, before);
}

// ---- 7: validation ------------------------------------------------------------

/// Plan case 7: each poison writes nothing and names the path it is about.
#[tokio::test]
async fn an_invalid_declaration_writes_nothing_and_names_the_path() {
    let w = World::fresh("project_db_sync_invalid").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    let rows = w.project_count().await;
    let state = w.org_rows().await;

    let wide = "w".repeat(256);
    let cases: Vec<(&str, Vec<DeclaredProject>, Vec<DeclaredProject>, String)> = vec![
        (
            "a bad path",
            vec![root()],
            vec![project("okapi-5d10/bad path")],
            "okapi-5d10/bad path".into(),
        ),
        (
            "a duplicate, case-folded",
            vec![root()],
            vec![project(P1), project(&P1.to_ascii_uppercase())],
            P1.to_ascii_uppercase(),
        ),
        (
            "a local/ project",
            vec![root()],
            vec![project("local/u1/thing")],
            "local/u1/thing".into(),
        ),
        (
            "the reserved root",
            vec![declared("local", "", REPO)],
            vec![],
            "\"local\"".into(),
        ),
        (
            "a two-segment root",
            vec![declared(P1, "", REPO)],
            vec![],
            P1.into(),
        ),
        ("no root", vec![], vec![project(P1)], "root".into()),
        (
            "a project under no declared root",
            vec![root()],
            vec![project("aardvark-9/one")],
            "aardvark-9/one".into(),
        ),
        (
            "a root declared again as a project",
            vec![root()],
            vec![project(R)],
            R.into(),
        ),
        (
            "an empty source_repo",
            vec![root()],
            vec![declared(P1, "", "")],
            P1.into(),
        ),
        (
            "a display_name wider than the column",
            vec![root()],
            vec![declared(P1, &wide, REPO)],
            P1.into(),
        ),
        (
            "a source_repo wider than the column",
            vec![root()],
            vec![declared(P1, "", &wide)],
            P1.into(),
        ),
    ];
    for (what, roots, projects, named) in cases {
        let err = w.sync(&roots, &projects, true).await.expect_err(what);
        assert_eq!(err.code(), Code::InvalidArgument, "{what}: {err:?}");
        assert!(
            err.message().contains(&named),
            "{what}: the refusal must name {named:?}: {}",
            err.message()
        );
        assert_eq!(w.project_count().await, rows, "{what} wrote a row");
        assert_eq!(w.org_rows().await, state, "{what} changed a row");
    }
}

/// A `display_name` and a `source_repo` AT the column width are stored — the
/// bound is characters, not bytes, as `RegisterProject`'s is.
#[tokio::test]
async fn values_at_the_column_width_are_stored() {
    let w = World::fresh("project_db_sync_width").await;
    let name = "\u{1F600}".repeat(255);
    let repo = "r".repeat(255);
    w.sync(&[root()], &[declared(P1, &name, &repo)], false)
        .await
        .expect("255 characters fit a VARCHAR(255)");
    assert_eq!(w.get(P1).await.unwrap().display_name, name);
}

#[tokio::test]
async fn a_sync_without_a_scope_or_a_principal_is_refused() {
    let w = World::fresh("project_db_sync_scope").await;
    let mut req = w.sync_request(&[root()], &[], false);
    req.scope = None;
    let err =
        w.db.sync_as(Some(yadgar_project_db::service::SYNC_CALLER), req)
            .await
            .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    // NO GATEWAY ON THIS PATH (ADR-0942): the refusal must not tell the
    // caller a gateway attests the Scope it was supposed to build.
    assert!(
        err.message().contains("seeder") && !err.message().contains("gateway"),
        "{}",
        err.message()
    );

    let mut req = w.sync_request(&[root()], &[], false);
    req.scope.as_mut().unwrap().user_id = String::new();
    let err =
        w.db.sync_as(Some(yadgar_project_db::service::SYNC_CALLER), req)
            .await
            .unwrap_err();
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(err.message().contains("user_id"), "{}", err.message());
    assert_eq!(w.project_count().await, 0);
}

// ---- 8: two roots ---------------------------------------------------------------

/// Plan case 8.
#[tokio::test]
async fn two_roots_are_two_org_rows_and_a_dropped_root_is_archived() {
    let w = World::fresh("project_db_sync_two_roots").await;
    let second = declared("okapi-top", "top group", "okapi-top/governance");
    let under_second = project("okapi-top/svc");
    w.sync(
        &[root(), second.clone()],
        &[project(P1), project(P2), under_second],
        false,
    )
    .await
    .unwrap();
    assert_eq!(w.stored_status("okapi-top").await, ACTIVE);
    assert_eq!(
        w.stored_class("okapi-top").await,
        (Some("okapi-top/governance".into()), None)
    );

    let r = w
        .sync(&[root()], &[project(P1), project(P2)], false)
        .await
        .unwrap();
    assert_eq!(counts(&r), (0, 0, 2, 0, 3));
    assert_eq!(w.stored_status("okapi-top").await, ARCHIVED);
    assert_eq!(w.stored_status("okapi-top/svc").await, ARCHIVED);
}

// ---- 9: aliases -----------------------------------------------------------------

/// Plan case 9.
#[tokio::test]
async fn a_declared_path_that_is_a_former_path_is_refused_and_writes_nothing() {
    let w = World::fresh("project_db_sync_alias").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    let id: String = sqlx::query_scalar("SELECT id FROM project WHERE path = ?")
        .bind(P1)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    w.seed_alias(P4, &id).await;
    let state = w.org_rows().await;

    let err = w
        .sync(&[root()], &[project(P1), project(P4)], false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::AlreadyExists, "{err:?}");
    assert!(err.message().contains(P4), "{}", err.message());
    assert_eq!(w.org_rows().await, state);
}

// ---- 10: the caller, before any transaction -------------------------------------

/// Plan case 10, the half that needs no handshake: a caller other than
/// `project-caller` — or no identity at all — is refused PERMISSION_DENIED
/// BEFORE a transaction opens.
///
/// The pool points at a port nothing listens on. A check that ran after
/// `begin()` would answer UNAVAILABLE or INTERNAL from the pool, never
/// PERMISSION_DENIED — so the code proves the order.
#[tokio::test]
async fn a_caller_that_is_not_project_is_refused_before_any_connection() {
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("mysql://nobody:nothing@127.0.0.1:1/nowhere")
        .unwrap();
    let db = ProjectDb::new(pool);
    let req = |w: &str| yadgar_project_db::pb::yadgar::project::v1::SyncProjectsRequest {
        scope: None,
        roots: vec![declared(w, "", REPO)],
        projects: vec![],
        allow_mass_archive: false,
    };
    for caller in [
        None,
        Some("task-caller"),
        Some("gateway-caller"),
        Some("seeder-caller"),
        Some("PROJECT-CALLER"),
        Some(""),
    ] {
        let err = db.sync_as(caller, req(R)).await.unwrap_err();
        assert_eq!(
            err.code(),
            Code::PermissionDenied,
            "{caller:?} must be refused before the pool: {err:?}"
        );
    }
}

// ---- 11: RegisterProject is the private path only --------------------------------

/// Plan case 11.
#[tokio::test]
async fn register_refuses_an_org_path_and_still_registers_a_private_one() {
    let w = World::fresh("project_db_sync_register_class").await;
    let err = w.try_register(P1, "").await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    assert!(err.message().contains("seeder"), "{}", err.message());
    assert_eq!(w.row_count(P1).await, 0);

    w.try_register("local/u1/okapi", "")
        .await
        .expect("a private path still registers");
}

// ---- 12: two first syncs ----------------------------------------------------------

/// Plan case 12. Starting EMPTY, two concurrent first syncs with different
/// declarations serialise: both succeed, one inserts everything, the other
/// converges onto its own declaration, and the store ends in exactly the
/// second one's state.
///
/// A THIRD SESSION HOLDS THE SENTINEL ROW while both syncs start, so both are
/// queued on the lock before either reads the empty table — the overlap is
/// forced rather than hoped for. Locking the org-class rows instead takes no
/// lock on an empty table, both syncs read "nothing there", both insert the
/// root, and one dies on `uq_project_path`.
#[tokio::test]
async fn two_first_syncs_into_an_empty_store_serialise_and_both_succeed() {
    let w = World::fresh("project_db_sync_first_race").await;
    let d1 = vec![project(P1), project(P2), project(P3)];
    let d2 = vec![project(P1), project(P2), project(P4)];

    let mut holder = w.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM project_sync_lock WHERE id = 1 FOR UPDATE")
        .execute(&mut *holder)
        .await
        .expect("the sentinel row exists from the migration");

    let roots = [root()];
    let first = w.sync(&roots, &d1, false);
    let second = w.sync(&roots, &d2, false);
    // RELEASED ONLY ONCE BOTH SYNCS ARE OBSERVED BLOCKED on the sentinel
    // statement, rather than after a guessed sleep.
    //
    // **READ FROM `PROCESSLIST`, NOT `INNODB_LOCK_WAITS`, AND THAT IS
    // MEASURED.** On `mariadb:11.8` the two waiting syncs sit in state
    // `Statistics` — the optimizer reads a primary-key point lookup as a const
    // table, so the row lock is requested during optimization — and neither
    // `INNODB_LOCK_WAITS` nor `INNODB_TRX` listed them after 10 s of waiting
    // (only the holder appeared). The processlist does show both, executing
    // the sentinel statement. The poller's own query names the table too, so
    // it excludes itself.
    let release = async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.PROCESSLIST
                  WHERE DB = DATABASE() AND ID <> CONNECTION_ID() AND COMMAND = 'Execute'
                    AND INFO LIKE 'SELECT id FROM project_sync_lock%FOR UPDATE'",
            )
            .fetch_one(&w.pool)
            .await
            .unwrap();
            if waiting >= 2 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "both syncs should be queued on the sentinel row; {waiting} waiting after 10s"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        holder.commit().await.unwrap();
    };
    let (r1, r2, ()) = tokio::join!(first, second, release);
    let (r1, r2) = (r1.expect("first sync"), r2.expect("second sync"));

    let mut outcomes = [counts(&r1), counts(&r2)];
    outcomes.sort();
    assert_eq!(
        outcomes,
        [(1, 0, 1, 0, 3), (4, 0, 0, 0, 0)],
        "one sync inserted the empty store's four rows; the other inserted one, archived one \
         and found three unchanged"
    );
    let winner = if r1.inserted == 4 { d2 } else { d1 };
    let mut want: Vec<String> = winner.iter().map(|p| p.path.clone()).collect();
    want.push(R.into());
    want.sort();
    assert_eq!(w.active_org_paths().await, want);
    assert_eq!(w.project_count().await, 5);
}

// ---- the mass-archive guard (ADR-0939) ------------------------------------------

#[tokio::test]
async fn archiving_more_than_half_of_the_active_org_rows_is_refused() {
    let w = World::fresh("project_db_sync_guard_trips").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    let state = w.org_rows().await;

    // Three of four active rows: strictly more than half.
    let err = w.sync(&[root()], &[], false).await.unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition, "{err:?}");
    for named in ["3", "4", "allow_mass_archive"] {
        assert!(err.message().contains(named), "{}", err.message());
    }
    assert_eq!(w.org_rows().await, state, "the guard writes nothing");
}

#[tokio::test]
async fn archiving_exactly_half_is_admitted() {
    let w = World::fresh("project_db_sync_guard_half").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    let r = w.sync(&[root()], &[project(P1)], false).await.unwrap();
    assert_eq!(counts(&r), (0, 0, 2, 0, 2));
}

#[tokio::test]
async fn the_flag_admits_a_mass_archive() {
    let w = World::fresh("project_db_sync_guard_flag").await;
    w.sync(&[root()], &three(), false).await.unwrap();
    let r = w.sync(&[root()], &[], true).await.unwrap();
    assert_eq!(counts(&r), (0, 0, 3, 0, 1));
}

/// The denominator is ACTIVE org-class rows: archived rows and private rows
/// count in neither half.
#[tokio::test]
async fn archived_and_private_rows_are_outside_the_guard_s_arithmetic() {
    let w = World::fresh("project_db_sync_guard_denominator").await;
    for n in 0..6 {
        w.try_register(&format!("local/u1/n{n}"), "").await.unwrap();
    }
    // Four active, then two archived: two active remain (root and P1).
    w.sync(&[root()], &three(), false).await.unwrap();
    w.sync(&[root()], &[project(P1)], false).await.unwrap();

    // Archiving P1 is one of two active rows — half, admitted. Counted against
    // all ten rows, or the four org rows, it would be a different answer only
    // if the guard used the wrong denominator in the other direction, so the
    // next step checks that direction too.
    let r = w.sync(&[root()], &[], false).await.unwrap();
    assert_eq!(r.archived, 1);

    // One active org row left (the root). Archiving it is one of one: refused,
    // though it is one of nine rows in the table.
    let err = w
        .sync(&[declared("okapi-other", "", REPO)], &[], false)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Code::FailedPrecondition);
}

// ---- case-only spelling -----------------------------------------------------------

/// A declared path differing from the stored one by ASCII case alone is the
/// SAME row (the column folds case). The stored spelling is kept and the row
/// counts unchanged: the path is the partition key every record carries, and
/// rewriting it would be a rename, which is out of scope (D53).
#[tokio::test]
async fn a_case_only_difference_is_the_same_row_and_keeps_its_spelling() {
    let w = World::fresh("project_db_sync_case").await;
    w.sync(&[root()], &three(), false).await.unwrap();

    let shouted = DeclaredProject {
        path: P1.to_ascii_uppercase(),
        ..project(P1)
    };
    let r = w
        .sync(&[root()], &[shouted, project(P2), project(P3)], false)
        .await
        .unwrap();
    assert_eq!(counts(&r), (0, 0, 0, 0, 4));
    assert_eq!(w.project_count().await, 4);
    let stored: String = sqlx::query_scalar("SELECT path FROM project WHERE path = ?")
        .bind(P1)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert_eq!(stored, P1);
}
