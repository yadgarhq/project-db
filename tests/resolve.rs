//! `ResolveProject` — the load-bearing rpc, and the one every scoped write in
//! the estate goes through.

mod support;
use support::*;

use yadgar_project_db::pb::yadgar::project::v1::ProjectStatus;

/// A database per test. `cargo test` runs the tests in one file concurrently,
/// so a shared name is two tests racing on one schema.
async fn world(name: &str) -> World {
    World::fresh(name).await
}

#[tokio::test]
async fn a_registered_path_resolves_to_itself_and_is_exact() {
    let w = world("project_db_resolve_exact").await;
    w.register(A).await;

    let r = w.resolve(A).await.expect("resolve");
    assert_eq!(r.resolved_path, A);
    assert!(r.exact, "a path with a row of its own is an exact match");
    assert!(!r.via_alias);
    assert_eq!(r.status, ProjectStatus::Active as i32);
}

/// D52 as amended by D53: an unregistered path lands in a real parent rather
/// than failing, and the softness is REPORTED rather than silent.
#[tokio::test]
async fn an_unregistered_path_resolves_to_its_nearest_registered_ancestor() {
    let w = world("project_db_resolve_ancestor").await;
    w.register(ROOT).await;
    w.register(A).await;

    let r = w
        .resolve(&format!("{A_DEEP}/never-registered"))
        .await
        .expect("resolve");

    assert_eq!(
        r.resolved_path, A,
        "the NEAREST ancestor, not the root — a walk that stopped at the first level it found \
         anything at would answer {ROOT} and file the work two levels too high"
    );
    assert!(
        !r.exact,
        "`exact` is what the caller surfaces as a D39 notice naming the id to register (D52); \
         reporting true here makes the soft failure silent"
    );
}

/// **THE FIXTURE A STRING-PREFIX IMPLEMENTATION FAILS, AND THE ONLY ONE.**
/// Every other test in this file passes for `path LIKE candidate%`.
#[tokio::test]
async fn a_sibling_sharing_a_prefix_is_never_an_ancestor() {
    let w = world("project_db_resolve_prefix").await;
    w.register(ROOT).await;
    // `.../alpha` and `.../alphax` differ by one character and are different
    // projects. A prefix match makes the first an ancestor of the second's
    // children; segment-wise resolution does not.
    w.register(A).await;

    let r = w.resolve(&format!("{A}x/service")).await.expect("resolve");

    assert_eq!(
        r.resolved_path, ROOT,
        "{A} is not an ancestor of {A}x/service — it merely shares its first characters. \
         Resolving there would file one project's work under another's"
    );
    assert!(!r.exact);
}

#[tokio::test]
async fn nothing_registered_anywhere_up_the_chain_is_a_refusal_and_creates_nothing() {
    let w = world("project_db_resolve_nothing").await;
    w.register(B).await;

    let err = w
        .resolve(&format!("{ROOT}-other/thing"))
        .await
        .expect_err("no ancestor is registered");
    assert_eq!(err.code(), tonic::Code::NotFound);

    // **NOTHING IS EVER AUTO-CREATED (D52).** The refusal is only half the
    // property; the other half is that the failed resolution left no row behind
    // for the next call to find.
    assert!(
        w.list("").await.iter().all(|p| p.path == B),
        "a resolution that refused must not have created a project"
    );
}

/// A rename is an alias, never a rewrite (D53), so the old path goes on
/// resolving.
#[tokio::test]
async fn a_former_path_resolves_to_the_project_and_says_so() {
    let w = world("project_db_resolve_alias").await;
    let id = w.register(B).await;
    // Seeded: `RenameProject` is held back in this release, and the alias read
    // paths are contract obligations regardless — see `World::seed_alias`.
    w.seed_alias(A, &id).await;

    let r = w.resolve(A).await.expect("resolve");
    assert_eq!(r.resolved_path, B, "the alias resolves to the live path");
    assert!(
        r.exact,
        "`exact` is false only when an ANCESTOR was used; the candidate had a row of its own \
         here, in the alias table"
    );
    assert!(
        r.via_alias,
        "the contract sets this when candidate_path matched an alias rather than a live path"
    );
}

/// **THE PROPERTY A RENAME WOULD OTHERWISE BREAK FOR AN ENTIRE SUBTREE.**
/// Records under a renamed parent still carry the old parent's path, so a walk
/// that consulted live paths only would strand every descendant — the rewrite
/// the alias exists to avoid, arriving as a resolution failure instead.
#[tokio::test]
async fn an_ancestor_reached_through_an_alias_still_resolves() {
    let w = world("project_db_resolve_alias_ancestor").await;
    let id = w.register(B).await;
    w.seed_alias(A, &id).await;

    let r = w
        .resolve(&format!("{A}/still-pointing-at-the-old-parent"))
        .await
        .expect("resolve");

    assert_eq!(r.resolved_path, B);
    assert!(!r.exact, "an ancestor was used");
    assert!(
        !r.via_alias,
        "the contract scopes via_alias to the CANDIDATE — 'set when candidate_path matched an \
         alias' — and this candidate matched nothing at all. Reporting true would be a statement \
         about a different path than the one the caller sent"
    );
}

/// An exact live path beats an ancestor, however deep the ancestor chain is.
#[tokio::test]
async fn a_deeper_registration_wins_over_its_own_ancestors() {
    let w = world("project_db_resolve_deepest").await;
    w.register(ROOT).await;
    w.register(A).await;
    w.register(A_DEEP).await;

    let r = w.resolve(A_DEEP).await.expect("resolve");
    assert_eq!(r.resolved_path, A_DEEP);
    assert!(r.exact);
}

/// Archiving is a STATUS, not a deletion: the path goes on resolving and the
/// caller is told what it resolved to.
#[tokio::test]
async fn an_archived_project_still_resolves_and_reports_its_status() {
    let w = world("project_db_resolve_archived").await;
    w.register(A).await;
    // Seeded: `ArchiveProject` is held back in this release, and what a resolve
    // REPORTS for an archived project is a contract obligation regardless.
    w.seed_status(A, ProjectStatus::Archived).await;

    let r = w.resolve(A).await.expect("resolve");
    assert_eq!(r.resolved_path, A);
    assert_eq!(
        r.status,
        ProjectStatus::Archived as i32,
        "refusing here would make every record ever written under this path unreadable; the \
         contract carries `status` so the caller decides"
    );
}

#[tokio::test]
async fn a_candidate_that_is_not_a_project_path_is_refused_before_any_lookup() {
    let w = world("project_db_resolve_bad_path").await;
    w.register(ROOT).await;

    // The measured class: a DESCRIPTION filled into a required identity
    // parameter. Resolving it upward would file prose under a real project.
    let err = w
        .resolve("debugging opsecrets nixos-quinyx")
        .await
        .expect_err("prose is not a path");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn an_absent_scope_is_refused() {
    let w = world("project_db_resolve_no_scope").await;
    w.register(A).await;

    use tonic::Request;
    use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;
    use yadgar_project_db::pb::yadgar::project::v1::ResolveProjectRequest;

    let err =
        w.db.resolve_project(Request::new(ResolveProjectRequest {
            scope: None,
            candidate_path: A.into(),
        }))
        .await
        .expect_err("scope is attested by the gateway and is never absent");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}
