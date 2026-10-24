//! Team grants on org rows, written by `SyncProjects` (ADR-0962, ADR-0963 (1)
//! and (6), `plans/team-access.md` stage 4). The numbered cases are the plan's
//! stage 4 reds.
//!
//! Every case runs on its own database, for the reason `tests/sync.rs` gives:
//! a sync archives every org row it is not told about.

mod support;
use support::*;

use tonic::Code;
use yadgar_project_db::pb::yadgar::project::v1::{
    DeclaredGrant, DeclaredProject, GrantLevel, ProjectStatus, SyncProjectsResponse,
};

/// The sentinel root of this suite.
const R: &str = "tapir-4e81";
const P1: &str = "tapir-4e81/one";
const P2: &str = "tapir-4e81/two";
const P3: &str = "tapir-4e81/three";
const REPO: &str = "tapir-4e81/registry";

/// Team ids, as the seeder derives them from declared names.
const ADMINS: &str = "yadgar:team:administrators";
const RUNNERS: &str = "yadgar:team:estate-runners";
const READERS: &str = "yadgar:team:readers";
const WRITERS: &str = "yadgar:team:writers";

const RW: GrantLevel = GrantLevel::ReadWrite;
const RO: GrantLevel = GrantLevel::Read;

fn grant(team: &str, level: GrantLevel) -> DeclaredGrant {
    DeclaredGrant {
        team_id: team.into(),
        level: level as i32,
    }
}

fn with(mut d: DeclaredProject, grants: Vec<DeclaredGrant>) -> DeclaredProject {
    d.grants = grants;
    d
}

fn root(grants: Vec<DeclaredGrant>) -> DeclaredProject {
    with(declared(R, "tapir", REPO), grants)
}

fn project(path: &str) -> DeclaredProject {
    declared(path, &format!("name of {path}"), &format!("{path}-repo"))
}

fn three() -> Vec<DeclaredProject> {
    vec![project(P1), project(P2), project(P3)]
}

/// `(declared, inserted, deleted, unchanged)`.
fn grant_counts(r: &SyncProjectsResponse) -> (u32, u32, u32, u32) {
    (
        r.grants_declared,
        r.grants_inserted,
        r.grants_deleted,
        r.grants_unchanged,
    )
}

fn level(l: GrantLevel) -> i8 {
    l as i8
}

// ---- 1, 2: insert, then converge -------------------------------------------

/// Stage 4 red 1.
#[tokio::test]
async fn red_01_a_root_grant_and_three_ungranted_projects_insert_one_grant() {
    let w = World::fresh("project_db_grants_insert").await;
    let r = w
        .sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .expect("sync");
    assert_eq!(grant_counts(&r), (1, 1, 0, 0));
    assert_eq!(
        w.grant_rows().await,
        vec![(R.to_string(), ADMINS.to_string(), level(RW))]
    );
    // The rows themselves count as before: grants are not rows.
    assert_eq!(r.inserted, 4);
}

/// Stage 4 red 2.
#[tokio::test]
async fn red_02_the_same_declaration_again_leaves_the_grant_and_the_version() {
    let w = World::fresh("project_db_grants_converge").await;
    let decl = [root(vec![grant(ADMINS, RW)])];
    w.sync(&decl, &three(), false).await.unwrap();
    let before = w.org_rows().await;

    let r = w.sync(&decl, &three(), false).await.unwrap();
    assert_eq!(grant_counts(&r), (1, 0, 0, 1));
    assert_eq!(r.unchanged, 4);
    assert_eq!(w.org_rows().await, before, "no version moved");
}

// ---- 3: level change ---------------------------------------------------------

/// Stage 4 red 3.
#[tokio::test]
async fn red_03_a_level_change_rewrites_the_one_grant_row() {
    let w = World::fresh("project_db_grants_level").await;
    w.sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .unwrap();
    let before = w.org_rows().await;

    let r = w
        .sync(&[root(vec![grant(ADMINS, RO)])], &three(), false)
        .await
        .unwrap();
    // Neither inserted nor deleted nor unchanged: rewritten in place.
    assert_eq!(grant_counts(&r), (1, 0, 0, 0));
    assert_eq!(
        w.grant_rows().await,
        vec![(R.to_string(), ADMINS.to_string(), level(RO))]
    );
    assert_eq!(
        w.org_rows().await,
        before,
        "a grant change bumps no version"
    );
}

// ---- 4: archive --------------------------------------------------------------

/// Stage 4 red 4.
#[tokio::test]
async fn red_04_a_dropped_project_is_archived_and_its_own_grants_go_the_root_s_stay() {
    let w = World::fresh("project_db_grants_archive").await;
    let p3 = with(project(P3), vec![grant(RUNNERS, RO)]);
    w.sync(
        &[root(vec![grant(ADMINS, RW)])],
        &[project(P1), project(P2), p3],
        false,
    )
    .await
    .unwrap();
    assert_eq!(w.grant_rows().await.len(), 2);

    let r = w
        .sync_revoking(
            &[root(vec![grant(ADMINS, RW)])],
            &[project(P1), project(P2)],
            // 1 of 2 deleted is exactly half: admitted without the flag.
            false,
        )
        .await
        .unwrap();
    assert_eq!(r.archived, 1);
    assert_eq!(w.stored_status(P3).await, ProjectStatus::Archived as i8);
    assert_eq!(grant_counts(&r), (1, 0, 1, 1));
    assert_eq!(
        w.grant_rows().await,
        vec![(R.to_string(), ADMINS.to_string(), level(RW))]
    );
}

// ---- 5: the mass-revoke guard -----------------------------------------------

fn four_grants() -> (Vec<DeclaredProject>, Vec<DeclaredProject>) {
    (
        vec![root(vec![grant(ADMINS, RW), grant(RUNNERS, RW)])],
        vec![
            with(project(P1), vec![grant(READERS, RO)]),
            with(project(P2), vec![grant(WRITERS, RW)]),
            project(P3),
        ],
    )
}

/// Stage 4 red 5, the refusal.
#[tokio::test]
async fn red_05_deleting_three_of_four_grants_is_refused_and_writes_nothing() {
    let w = World::fresh("project_db_grants_mass_revoke").await;
    let (roots, projects) = four_grants();
    w.sync(&roots, &projects, false).await.unwrap();
    let grants = w.grant_rows().await;
    let rows = w.org_rows().await;
    assert_eq!(grants.len(), 4);

    let err = w
        .sync_revoking(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .expect_err("3 of 4 is more than half");
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert!(
        err.message().contains("allow_mass_revoke"),
        "{}",
        err.message()
    );
    assert!(err.message().contains('3') && err.message().contains('4'));
    assert_eq!(w.grant_rows().await, grants, "grant count before = after");
    assert_eq!(w.org_rows().await, rows, "and no row moved either");
}

/// Stage 4 red 5, the flag.
#[tokio::test]
async fn red_05_the_flag_admits_the_mass_revoke() {
    let w = World::fresh("project_db_grants_mass_revoke_flag").await;
    let (roots, projects) = four_grants();
    w.sync(&roots, &projects, false).await.unwrap();

    let r = w
        .sync_revoking(&[root(vec![grant(ADMINS, RW)])], &three(), true)
        .await
        .expect("allowed");
    assert_eq!(grant_counts(&r), (1, 0, 3, 1));
    assert_eq!(w.grant_rows().await.len(), 1);
}

/// Stage 4 red 5, the boundary: exactly half is admitted.
#[tokio::test]
async fn red_05_deleting_exactly_half_is_admitted() {
    let w = World::fresh("project_db_grants_mass_revoke_half").await;
    let (roots, projects) = four_grants();
    w.sync(&roots, &projects, false).await.unwrap();

    let r = w
        .sync_revoking(
            &[root(vec![grant(ADMINS, RW), grant(RUNNERS, RW)])],
            &three(),
            false,
        )
        .await
        .expect("2 of 4 is half");
    assert_eq!(grant_counts(&r), (2, 0, 2, 2));
}

/// Stage 4 red 5, the accident the guard exists for: a declaration that
/// carries NO grants at all (an old seeder, a dropped proto3 field, a
/// mis-rendered values file) against a stored set is refused, not applied as
/// "revoke everything".
#[tokio::test]
async fn red_05_a_declaration_with_no_grants_cannot_wipe_the_stored_set() {
    let w = World::fresh("project_db_grants_mass_revoke_empty").await;
    let (roots, projects) = four_grants();
    w.sync(&roots, &projects, false).await.unwrap();
    let grants = w.grant_rows().await;
    assert_eq!(grants.len(), 4);

    let err = w
        .sync_revoking(&[root(vec![])], &three(), false)
        .await
        .expect_err("4 of 4 is more than half");
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(w.grant_rows().await, grants, "nothing was revoked");
}

/// Stage 4 red 5, the smallest store: ONE stored grant dropped is 1 of 1,
/// more than half, so a one-team install cannot lose its only grant by
/// accident either.
#[tokio::test]
async fn red_05_the_only_stored_grant_is_guarded_too() {
    let w = World::fresh("project_db_grants_mass_revoke_one").await;
    w.sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .unwrap();

    let err = w
        .sync_revoking(&[root(vec![])], &three(), false)
        .await
        .expect_err("1 of 1 is more than half");
    assert_eq!(err.code(), Code::FailedPrecondition);
    assert_eq!(w.grant_rows().await.len(), 1);
}

/// Stage 4 red 5, the other side: a store with no grants never trips the
/// guard, so the first sync that brings grants (and every grant-less sync
/// before it) is unaffected.
#[tokio::test]
async fn red_05_an_empty_store_never_trips_the_guard() {
    let w = World::fresh("project_db_grants_mass_revoke_none").await;
    w.sync(&[root(vec![])], &three(), false).await.unwrap();
    let r = w
        .sync_revoking(&[root(vec![])], &three(), false)
        .await
        .expect("nothing stored, nothing deleted");
    assert_eq!(grant_counts(&r), (0, 0, 0, 0));
}

// ---- 6: malformed grants ------------------------------------------------------

/// Stage 4 red 6: one case each, every refusal names the path and writes
/// nothing.
#[tokio::test]
async fn red_06_an_unspecified_level_a_duplicate_team_or_a_bad_team_id_is_refused() {
    let w = World::fresh("project_db_grants_invalid").await;
    w.sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .unwrap();
    let grants = w.grant_rows().await;
    let rows = w.org_rows().await;

    let unspecified = DeclaredGrant {
        team_id: READERS.into(),
        level: GrantLevel::Unspecified as i32,
    };
    let out_of_range = DeclaredGrant {
        team_id: READERS.into(),
        level: 7,
    };
    let cases: Vec<(&str, DeclaredProject)> = vec![
        ("unspecified level", with(project(P1), vec![unspecified])),
        ("unknown level", with(project(P1), vec![out_of_range])),
        (
            "duplicate team",
            with(project(P1), vec![grant(READERS, RO), grant(READERS, RW)]),
        ),
        ("no prefix", with(project(P1), vec![grant("readers", RO)])),
        (
            "upper case",
            with(project(P1), vec![grant("yadgar:team:Readers", RO)]),
        ),
        (
            "empty name",
            with(project(P1), vec![grant("yadgar:team:", RO)]),
        ),
        (
            "leading digit",
            with(project(P1), vec![grant("yadgar:team:1readers", RO)]),
        ),
        (
            "too long",
            with(
                project(P1),
                vec![grant(&format!("yadgar:team:a{}", "b".repeat(63)), RO)],
            ),
        ),
    ];
    for (what, p1) in cases {
        let err = w
            .sync(
                &[root(vec![grant(ADMINS, RW)])],
                &[p1, project(P2), project(P3)],
                false,
            )
            .await
            .expect_err(what);
        assert_eq!(err.code(), Code::InvalidArgument, "{what}");
        assert!(err.message().contains(P1), "{what}: {}", err.message());
        assert_eq!(w.grant_rows().await, grants, "{what}: nothing written");
        assert_eq!(w.org_rows().await, rows, "{what}: no row moved");
    }

    // The longest legal name is admitted: 1 + 62 characters.
    let longest = format!("yadgar:team:a{}", "b".repeat(62));
    w.sync(
        &[root(vec![grant(ADMINS, RW)])],
        &[
            with(project(P1), vec![grant(&longest, RO)]),
            project(P2),
            project(P3),
        ],
        false,
    )
    .await
    .expect("63 characters is the limit, and legal");
}

// ---- 7: private rows ----------------------------------------------------------

/// Stage 4 red 7.
#[tokio::test]
async fn red_07_a_private_row_is_untouched_and_returns_no_grants() {
    let w = World::fresh("project_db_grants_private").await;
    let private = "local/0192f3a4-5b6c-7d8e-9f01-23456789abc1/tapir-notes";
    w.try_register(private, "notes").await.expect("register");
    w.sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .unwrap();

    let got = w.get(private).await.expect("get");
    assert!(got.grants.is_empty());
    assert!(
        w.grant_rows().await.iter().all(|(p, _, _)| p != private),
        "no grant row names the private row"
    );
}

/// Stage 4 red 7, the store-defect arm: a grant row that names a PRIVATE row
/// (no write path can make one; a manual SQL write or a future bug could) is
/// never returned by a read, never counted by the mass-revoke guard and never
/// touched by a sync. Ruling 7: grants are never read for a `local/` row.
#[tokio::test]
async fn red_07_a_grant_row_on_a_private_row_is_never_read_and_never_synced() {
    let w = World::fresh("project_db_grants_private_defect").await;
    let private = "local/0192f3a4-5b6c-7d8e-9f01-23456789abc1/tapir-defect";
    w.try_register(private, "defect").await.expect("register");
    w.sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO project_grant (project_id, team_id, level)
         SELECT id, ?, 2 FROM project WHERE path = ?",
    )
    .bind(ADMINS)
    .bind(private)
    .execute(&w.pool)
    .await
    .expect("plant the defect row");

    assert!(w.get(private).await.expect("get").grants.is_empty());

    // 1 org grant stored; dropping it alone is 1 of 1 and refused. Were the
    // private defect row counted, 1 of 2 would pass and the root's only grant
    // would go.
    let err = w
        .sync_revoking(&[root(vec![])], &three(), false)
        .await
        .expect_err("the defect row is not in the guard's denominator");
    assert_eq!(err.code(), Code::FailedPrecondition);

    // A sync that keeps the org grant leaves the defect row where it is.
    w.sync(&[root(vec![grant(ADMINS, RW)])], &three(), false)
        .await
        .unwrap();
    assert!(
        w.grant_rows().await.iter().any(|(p, _, _)| p == private),
        "a sync never touches a private row's grant rows"
    );
}

// ---- the read paths ----------------------------------------------------------

/// `Project.grants` is filled on every read path that returns a `Project`:
/// GetProject and every page of ListProjects. A repeated field that no read
/// path fills decodes as empty, which is legal, so nothing else would notice.
#[tokio::test]
async fn get_and_every_list_page_carry_each_row_s_own_grants() {
    let w = World::fresh("project_db_grants_read").await;
    w.sync(
        &[root(vec![grant(ADMINS, RW), grant(RUNNERS, RO)])],
        &[
            with(project(P1), vec![grant(READERS, RO)]),
            project(P2),
            project(P3),
        ],
        false,
    )
    .await
    .unwrap();

    let root_row = w.get(R).await.unwrap();
    let teams: Vec<(&str, i32)> = root_row
        .grants
        .iter()
        .map(|g| (g.team_id.as_str(), g.level))
        .collect();
    assert_eq!(teams, vec![(ADMINS, RW as i32), (RUNNERS, RO as i32)]);
    assert!(w.get(P2).await.unwrap().grants.is_empty(), "not inherited");

    // Page size 1: every row on its own page.
    let mut token = String::new();
    let mut seen = Vec::new();
    loop {
        let page = w.list_page(R, None, 1, &token).await.unwrap();
        for p in &page.projects {
            seen.push((p.path.clone(), p.grants.len()));
        }
        if page.next_page_token.is_empty() {
            break;
        }
        token = page.next_page_token;
    }
    seen.sort();
    assert_eq!(
        seen,
        vec![
            (R.to_string(), 2),
            (P1.to_string(), 1),
            (P3.to_string(), 0),
            (P2.to_string(), 0),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
}
