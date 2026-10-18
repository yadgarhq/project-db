//! A project's OWNER on the read paths (ledger 1638, `plans/project-membership.md`
//! car P1).
//!
//! The gateway's membership check compares a private row's stored
//! `owner_user_id` with the caller. The owner reaches it through `ListProjects`
//! (relayed by `project` into `RegisteredPath.owner_user_id`), so this store
//! must report the stored owner on `Meta.owner_user_id` rather than blank it.
//!
//! **THE REGISTRANT IS [`U2`], NEVER THE FIXTURE DEFAULT [`U1`].** Every
//! request this fixture sends carries [`U1`] as the caller, so an
//! implementation that echoed the caller's `Scope.user_id` into the field would
//! satisfy an assertion about [`U1`]. Registering as [`U2`] and reading as
//! [`U1`] tells the stored owner apart from the reader.

mod support;
use support::*;

use yadgar_project_db::pb::yadgar::project::v1::Project;

/// A private-class path: `local/<account>/<path>` (ADR-0605).
const PRIVATE: &str = "local/okapi-5e12/notes";

fn owner(p: &Project) -> &str {
    p.meta.as_ref().expect("meta").owner_user_id.as_str()
}

fn by_path<'a>(projects: &'a [Project], path: &str) -> &'a Project {
    projects
        .iter()
        .find(|p| p.path == path)
        .unwrap_or_else(|| panic!("{path} is not in the listed page"))
}

#[tokio::test]
async fn list_reports_a_private_rows_owner_and_an_empty_owner_on_an_org_row() {
    let w = World::fresh("owner_list").await;
    w.register(A).await;
    w.register_as(PRIVATE, "notes", U2, None)
        .await
        .expect("register the private row as U2");

    let all = w.list("").await;
    assert_eq!(
        owner(by_path(&all, PRIVATE)),
        U2,
        "a private row reports its registrant as its owner, not the reader"
    );
    assert_eq!(
        owner(by_path(&all, A)),
        "",
        "an org row has no owner (ck_project_class_path keeps it NULL)"
    );
}

#[tokio::test]
async fn get_reports_the_same_owner_list_reports_for_both_classes() {
    let w = World::fresh("owner_get").await;
    w.register(A).await;
    w.register_as(PRIVATE, "notes", U2, None)
        .await
        .expect("register the private row as U2");

    let all = w.list("").await;
    for path in [PRIVATE, A] {
        let got = w.get(path).await.expect("get");
        assert_eq!(
            owner(&got),
            owner(by_path(&all, path)),
            "GetProject and ListProjects disagree on the owner of {path}"
        );
    }
    assert_eq!(owner(&w.get(PRIVATE).await.expect("get")), U2);
}

/// `RegisterProject` writes private rows only (ADR-0936), and the owner it
/// stores is the caller. Its answer reports that owner, so a registrant never
/// sees a `Meta` that disagrees with what `GetProject` returns a moment later.
#[tokio::test]
async fn the_register_answer_reports_the_owner_it_stored() {
    let w = World::fresh("owner_register").await;
    let answer = w
        .register_as(PRIVATE, "notes", U2, None)
        .await
        .expect("register the private row as U2");
    let meta = answer.meta.expect("meta");
    assert_eq!(
        meta.owner_user_id, U2,
        "the register answer names its owner"
    );
    assert_eq!(
        meta.owner_user_id,
        owner(&w.get(PRIVATE).await.expect("get")),
        "the register answer and GetProject disagree on the owner"
    );
}
