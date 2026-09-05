//! `RegisterProject` and `GetProject`, the namespace rule that spans two tables,
//! and the two verbs this release refuses.

mod support;
use support::*;

use yadgar_project_db::pb::yadgar::project::v1::ProjectStatus;

async fn world(name: &str) -> World {
    World::fresh(name).await
}

#[tokio::test]
async fn a_registration_is_active_in_the_column_and_at_version_one() {
    let w = world("project_db_registry_register").await;
    let meta = w
        .try_register(A, "Alpha")
        .await
        .expect("register")
        .meta
        .expect("meta");

    assert_eq!(meta.version, 1);
    assert_eq!(
        meta.project_id, A,
        "a project's own partition key is itself"
    );
    assert!(
        meta.id.starts_with("yadgar:project:"),
        "the id is a URN and never the engine's key (D42): {}",
        meta.id
    );
    // READ FROM THE COLUMN. `RegisterProjectRequest` carries no status field, so
    // an implementation that never wrote one would leave a zero here —
    // PROJECT_STATUS_UNSPECIFIED — and every `ListProjects` filtered on ACTIVE
    // would silently miss it.
    assert_eq!(w.stored_status(A).await, ProjectStatus::Active as i8);

    let project = w.get(A).await.expect("get");
    assert_eq!(project.display_name, "Alpha");
    assert_eq!(project.status, ProjectStatus::Active as i32);
    assert!(project.aliases.is_empty());
    assert!(
        project.last_seen_at.is_none(),
        "nothing has run under it yet, and that is a different fact from 'seen at the epoch'"
    );
    assert!(
        project.meta.as_ref().and_then(|m| m.created_at).is_some(),
        "created_at must survive the trip — no Rust type here decodes a TIMESTAMP, so a bare \
         SELECT would have failed at decode or left this absent"
    );
}

#[tokio::test]
async fn one_path_is_one_project() {
    let w = world("project_db_registry_duplicate").await;
    w.register(A).await;

    let err = w
        .try_register(A, "a second claim on one path")
        .await
        .expect_err("two rows claiming one path is the split-corpus failure");
    assert_eq!(err.code(), tonic::Code::AlreadyExists);
}

/// **THE NAMESPACE RULE NO SINGLE CONSTRAINT CAN STATE.** An alias and a live
/// path are two tables, so a unique index cannot span them.
#[tokio::test]
async fn a_former_path_cannot_be_registered_as_a_new_project() {
    let w = world("project_db_registry_alias_collision").await;
    let id = w.register(B).await;
    // Seeded, because `RenameProject` is held back in this release — see
    // `World::seed_alias`.
    w.seed_alias(A, &id).await;

    let err = w
        .try_register(A, "a new project at a former path")
        .await
        .expect_err("one path must not mean two things");
    assert_eq!(err.code(), tonic::Code::AlreadyExists);

    // The refusal is only half of it: the alias must still resolve where it did.
    assert_eq!(w.resolve(A).await.expect("resolve").resolved_path, B);
}

/// `Get` follows a former path; `Resolve` follows a former path AND an ancestor.
/// Both halves are pinned, because the distinction is the reason both rpcs exist.
#[tokio::test]
async fn get_follows_a_former_path_but_never_an_ancestor() {
    let w = world("project_db_registry_get").await;
    let id = w.register(B).await;
    w.seed_alias(A, &id).await;

    let by_former = w
        .get(A)
        .await
        .expect("a former path still names the project");
    assert_eq!(by_former.path, B);
    assert_eq!(
        by_former.aliases,
        vec![A.to_string()],
        "a read path that never filled `aliases` would return an empty vec here — a legal value, \
         with nothing anywhere reporting it"
    );

    // An ancestor names a DIFFERENT project, so answering with it would be a
    // substitution rather than a lookup.
    let err = w
        .get(&format!("{B}/unregistered-child"))
        .await
        .expect_err("Get does not walk upward");
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// **THE TWO VERBS THIS RELEASE HOLDS BACK, AND THE REASON IS IN THE MESSAGE.**
/// A method a service silently lacks answers `UNIMPLEMENTED` from tonic's own
/// fallback with no reason in it, and a caller cannot tell that from a version
/// skew or a bad route.
#[tokio::test]
async fn rename_and_archive_refuse_and_say_why() {
    let w = world("project_db_registry_held_back").await;
    w.register(A).await;

    for err in [
        w.rename(A, B, 1).await.expect_err("rename is held back"),
        w.archive(A, 1).await.expect_err("archive is held back"),
    ] {
        assert_eq!(err.code(), tonic::Code::Unimplemented);
        let message = err.message();
        // The REASON, not merely a refusal. A caller reading this has to learn
        // that the block is about records rather than about the verb, or the
        // obvious response is to retry it later against the same estate.
        for expected in ["retag", "RenameProject and ArchiveProject return together"] {
            assert!(
                message.contains(expected),
                "the refusal no longer carries {expected:?}: {message}"
            );
        }
    }

    // AND NOTHING MOVED. A refusal that had already written would be worse than
    // one that had not refused at all.
    assert_eq!(w.stored_version(A).await, 1);
    assert_eq!(w.stored_status(A).await, ProjectStatus::Active as i8);
    assert_eq!(w.alias_count().await, 0);
    assert_eq!(w.get(A).await.expect("get").path, A);
}

#[tokio::test]
async fn a_display_name_wider_than_the_column_is_refused_rather_than_truncated() {
    let w = world("project_db_registry_display_name").await;
    let err = w
        .try_register(A, &"n".repeat(256))
        .await
        .expect_err("wider than the column");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // EMPTY IS LEGITIMATE, and refusing it would forbid an ordinary
    // registration whose name is its path.
    w.try_register(B, "")
        .await
        .expect("an empty display name is a name");
}

#[tokio::test]
async fn a_path_that_is_not_a_project_path_registers_nothing() {
    let w = world("project_db_registry_bad_path").await;
    for bad in [
        "",
        "/leading",
        "trailing/",
        "a//b",
        "has space/x",
        "up/../x",
    ] {
        let err = w
            .try_register(bad, "x")
            .await
            .expect_err("not a project path");
        assert_eq!(err.code(), tonic::Code::InvalidArgument, "{bad:?}");
    }
    assert!(w.list("").await.is_empty(), "nothing was created");
}

/// **THE PRIVATE CLASS KEYS ON A FULL DIRECTORY PATH**, not on a basename:
/// `local/<basename>` collides between two unrelated directories of the same
/// name. So the paths this store must accept are DEEP, and the client trims for
/// length rather than for depth — which is why `src/path.rs` implements no depth
/// cap and bounds a path by the width of the column instead.
#[tokio::test]
async fn a_deep_path_derived_from_a_full_directory_is_registrable() {
    let w = world("project_db_registry_deep").await;
    let deep = format!("local/home/max/git/{ROOT}/alpha/services/forecast");
    w.try_register(&deep, "")
        .await
        .expect("a deep path is a path");

    assert_eq!(w.get(&deep).await.expect("get").path, deep);
    // And it still resolves upward through every one of those segments.
    let r = w
        .resolve(&format!("{deep}/unregistered"))
        .await
        .expect("resolve");
    assert_eq!(r.resolved_path, deep);
    assert!(!r.exact);
}
