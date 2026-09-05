//! `ListProjects` — the subtree axis (D53), the page, and the status filter.

mod support;
use support::*;

use yadgar_project_db::pb::yadgar::project::v1::ProjectStatus;

async fn world(name: &str) -> World {
    World::fresh(name).await
}

fn paths(projects: &[yadgar_project_db::pb::yadgar::project::v1::Project]) -> Vec<String> {
    projects.iter().map(|p| p.path.clone()).collect()
}

#[tokio::test]
async fn an_empty_under_path_lists_every_project() {
    let w = world("project_db_list_all").await;
    w.register(ROOT).await;
    w.register(A).await;
    w.register(B).await;

    assert_eq!(
        paths(&w.list("").await),
        vec![ROOT.to_string(), A.to_string(), B.to_string()],
        "ordered by path, which is the keyset sort key"
    );
}

/// D53: a query at a path sees that path AND every descendant.
#[tokio::test]
async fn a_subtree_includes_the_named_project_and_everything_beneath_it() {
    let w = world("project_db_list_subtree").await;
    w.register(ROOT).await;
    w.register(A).await;
    w.register(A_DEEP).await;
    w.register(B).await;

    assert_eq!(
        paths(&w.list(A).await),
        vec![A.to_string(), A_DEEP.to_string()],
        "the named project is IN its own subtree; a LIKE pattern alone matches only what is \
         strictly beneath it"
    );
}

/// **THE FIXTURE A PREFIX MATCH FAILS.** `.../alpha` and `.../alphax` differ by
/// one character and are different projects.
#[tokio::test]
async fn a_sibling_sharing_a_prefix_is_not_in_the_subtree() {
    let w = world("project_db_list_prefix").await;
    w.register(A).await;
    let sibling = format!("{A}x");
    w.register(&sibling).await;

    assert_eq!(
        paths(&w.list(A).await),
        vec![A.to_string()],
        "{sibling} merely shares {A}'s first characters. A pattern without the separating slash \
         would list it as a descendant"
    );
}

/// **THE COLLISION IS REACHABLE WITH TWO LEGAL PATHS.** `_` is admitted by the
/// grammar because repository names contain it, and it is a LIKE metacharacter
/// matching any single character.
#[tokio::test]
async fn an_underscore_in_a_path_is_not_a_wildcard() {
    let w = world("project_db_list_underscore").await;
    let with_underscore = format!("{ROOT}/a_b");
    let collides = format!("{ROOT}/axb");
    w.register(&with_underscore).await;
    w.register(&collides).await;
    w.register(&format!("{with_underscore}/child")).await;
    w.register(&format!("{collides}/child")).await;

    assert_eq!(
        paths(&w.list(&with_underscore).await),
        vec![with_underscore.clone(), format!("{with_underscore}/child")],
        "an unescaped underscore makes {collides} and its children members of {with_underscore}'s \
         subtree — a project nobody put there"
    );
}

#[tokio::test]
async fn an_absent_status_lists_every_status_and_a_named_one_filters() {
    let w = world("project_db_list_status").await;
    w.register(A).await;
    w.register(B).await;
    // Seeded: `ArchiveProject` is held back in this release, and the status
    // filter is a contract obligation regardless.
    w.seed_status(B, ProjectStatus::Archived).await;

    assert_eq!(w.list("").await.len(), 2, "no filter means every status");
    assert_eq!(
        paths(
            &w.list_page("", Some(ProjectStatus::Active), 0, "")
                .await
                .expect("list")
                .projects
        ),
        vec![A.to_string()]
    );
    assert_eq!(
        paths(
            &w.list_page("", Some(ProjectStatus::Archived), 0, "")
                .await
                .expect("list")
                .projects
        ),
        vec![B.to_string()]
    );
}

/// An explicitly-sent UNSPECIFIED asked for something. Answering it with "every
/// status" would silently widen a filter rather than report that it named
/// nothing.
#[tokio::test]
async fn an_explicit_unspecified_status_is_refused_rather_than_widened() {
    let w = world("project_db_list_unspecified").await;
    w.register(A).await;

    let err = w
        .list_page("", Some(ProjectStatus::Unspecified), 0, "")
        .await
        .expect_err("UNSPECIFIED names no status");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn a_page_carries_a_token_only_when_there_is_another_page() {
    let w = world("project_db_list_paging").await;
    for n in 0..5 {
        w.register(&format!("{ROOT}/p{n}")).await;
    }

    let first = w.list_page("", None, 2, "").await.expect("list");
    assert_eq!(
        paths(&first.projects),
        vec![format!("{ROOT}/p0"), format!("{ROOT}/p1")]
    );
    assert_eq!(first.next_page_token, format!("{ROOT}/p1"));

    let second = w
        .list_page("", None, 2, &first.next_page_token)
        .await
        .expect("list");
    assert_eq!(
        paths(&second.projects),
        vec![format!("{ROOT}/p2"), format!("{ROOT}/p3")]
    );

    let last = w
        .list_page("", None, 2, &second.next_page_token)
        .await
        .expect("list");
    assert_eq!(paths(&last.projects), vec![format!("{ROOT}/p4")]);
    assert!(
        last.next_page_token.is_empty(),
        "a token on the final page reads as 'there is more' and costs the caller a round trip to \
         find out otherwise"
    );
}

#[tokio::test]
async fn a_page_size_the_caller_did_not_set_is_not_an_empty_page() {
    let w = world("project_db_list_default_page").await;
    w.register(A).await;

    // 0 is the wire default, and returning nothing for it would look like an
    // empty registry (D56 bounds the other end).
    assert_eq!(
        w.list_page("", None, 0, "")
            .await
            .expect("list")
            .projects
            .len(),
        1
    );
}

/// **THE DEFECT THIS ESTATE HAS ALREADY SHIPPED ONCE**, in `task-db`: a
/// repeated field the read path never fills decodes as empty, which is a legal
/// value, so nothing reports a problem. Asserted on LIST as well as on GET,
/// because the two take different code paths to the same field.
#[tokio::test]
async fn a_page_carries_every_projects_former_paths() {
    let w = world("project_db_list_aliases").await;
    let id = w.register(B).await;
    // TWO former paths, seeded — `RenameProject` is held back in this release.
    // Two rather than one, because a bucketing bug that assigned every alias to
    // the first project in the page is invisible with one.
    w.seed_alias(A, &id).await;
    w.seed_alias(A_DEEP, &id).await;
    w.register(ROOT).await;

    let listed = w.list("").await;
    let renamed = listed
        .iter()
        .find(|p| p.path == B)
        .expect("the renamed project is in the page");
    assert_eq!(
        renamed.aliases,
        vec![A.to_string(), A_DEEP.to_string()],
        "both former paths, ordered, so an otherwise identical response does not differ between \
         calls"
    );

    let untouched = listed
        .iter()
        .find(|p| p.path == ROOT)
        .expect("the never-renamed project is in the page");
    assert!(untouched.aliases.is_empty());
}

#[tokio::test]
async fn an_under_path_that_is_not_a_project_path_is_refused() {
    let w = world("project_db_list_bad_under").await;
    w.register(A).await;

    let err = w
        .list_page("not a path", None, 0, "")
        .await
        .expect_err("prose is not a path");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}
