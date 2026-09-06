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

/// **THE NUMBERS ARE LITERALS AND NOT `MAX_DISPLAY_NAME_CHARS` (ADR-0573).**
/// The bound is derived from an external artefact — the `VARCHAR(255)` in
/// migration 1 — so a test spelling it as the implementation's own constant
/// passes for whatever the constant happens to say, which is exactly how the
/// byte bound this test now pins survived review. Move the constant to 254 or to
/// 300 and the assertions below go red.
///
/// **THE MULTI-BYTE CASE IS THE ONE THAT PINS CHARACTERS RATHER THAN BYTES.**
/// 255 × `U+1F600` is 255 characters and 1020 bytes: the column stores it —
/// measured on `mariadb:11.8.9` — and a byte check refuses it. Every ASCII
/// assertion here passes under both readings and proves nothing about which one
/// the service uses.
#[tokio::test]
async fn a_display_name_wider_than_the_column_is_refused_rather_than_truncated() {
    let w = world("project_db_registry_display_name").await;

    // A NAME NO ASCII TEST DISTINGUISHES. `.len()` counts 1020 here and refuses;
    // the column counts 255 and stores.
    let wide = "\u{1F600}".repeat(255);
    w.try_register(&format!("{ROOT}/wide-name"), &wide)
        .await
        .expect("255 characters is what the column holds, whatever they cost in bytes");
    assert_eq!(
        w.get(&format!("{ROOT}/wide-name"))
            .await
            .expect("get")
            .display_name,
        wide,
        "and it must have been STORED rather than merely not refused — under a permissive \
         sql_mode the engine clips to 255 characters and reports success"
    );

    // ONE CHARACTER PAST THE COLUMN, in both alphabets. 256 × U+1F600 is
    // ERROR 1406 on the engine, and so is 256 × 'n'.
    for (what, name) in [
        ("256 characters of ASCII", "n".repeat(256)),
        ("256 characters of emoji", "\u{1F600}".repeat(256)),
    ] {
        let err = w
            .try_register(&format!("{ROOT}/too-wide"), &name)
            .await
            .expect_err(what);
        assert_eq!(err.code(), tonic::Code::InvalidArgument, "{what}");
    }

    // AND THE ASCII BOUND ITSELF, so that widening the check cannot pass
    // unnoticed either.
    w.try_register(&format!("{ROOT}/at-the-bound"), &"n".repeat(255))
        .await
        .expect("255 characters of ASCII is the column's width, not one past it");

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

/// **THE ONE-WAY DOOR THIS TEST HOLDS SHUT.** `local` is the first segment of
/// the private class above, so a project registered AT it becomes the nearest
/// registered ancestor of every private path in the estate — and every one of
/// them then resolves into it with `exact: false`, stamping one stranger's
/// partition key on all of them. Registration is immutable here, because
/// `RenameProject` is held back, so a `local` once claimed could never be
/// renamed away.
///
/// The literals are spelled out rather than read from `path::RESERVED_ROOT`
/// (ADR-0573): a test that references the constant under test moves with it and
/// pins nothing.
#[tokio::test]
async fn the_reserved_private_class_root_is_not_registrable() {
    let w = world("project_db_registry_reserved_root").await;

    let err = w
        .try_register("local", "an organisation called local")
        .await
        .expect_err("the root of the private class is not an organisation anybody may own");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message().contains("local"),
        "a refusal names the value it refused rather than rewriting it (ADR-0569): {}",
        err.message()
    );
    assert!(w.list("").await.is_empty(), "nothing was created");

    // AND THE CLASS THE SEGMENT PROTECTS IS UNTOUCHED. A guard that reserved the
    // PREFIX rather than the SEGMENT would pass every assertion above and delete
    // the private class outright, which is the mutant this arm exists to catch.
    w.try_register("local/home/max/src/alpha", "")
        .await
        .expect("a project beneath the reserved segment IS the private class");
    w.try_register("locals", "a different organisation entirely")
        .await
        .expect("`locals` merely shares five characters; a prefix is not a segment");
}

/// **THE COLLATION IS WHAT MAKES A BYTE-EXACT REFUSAL INSUFFICIENT.**
/// `project.path` is declared with no `COLLATE` (`src/schema.rs`, migration 1),
/// so it takes the server default — measured `utf8mb4_uca1400_ai_ci` on a live
/// MariaDB 11.8, which is case-insensitive. `uq_project_path` therefore holds ONE
/// slot for every ASCII-case spelling of the segment: with a byte comparison in
/// the guard, `RegisterProject("LOCAL")` answers OK, that row takes the reserved
/// slot, and nothing can retire it — `src/write.rs` holds both `RenameProject`
/// and `ArchiveProject` at `UNIMPLEMENTED` in this release. The door the
/// reservation exists to hold shut would still be open, one shift key away.
///
/// **THE SECOND ARM IS WHAT MEASURES THE COLLATION RATHER THAN ASSUMING IT.**
/// With a row already at `local`, a byte-comparing guard reaches the INSERT and
/// the ENGINE refuses it as `ALREADY_EXISTS` — and that answer IS the
/// measurement, the unique index saying the two spellings are one key. This
/// service must answer `INVALID_ARGUMENT` and never get there.
///
/// The spellings are literals rather than read from `path::RESERVED_ROOT`
/// (ADR-0573).
#[tokio::test]
async fn an_upper_case_spelling_of_the_reserved_root_is_refused_too() {
    let w = world("project_db_registry_reserved_root_case").await;

    for sent in ["LOCAL", "LoCaL"] {
        let err = w
            .try_register(sent, "an organisation called local")
            .await
            .expect_err("the collation makes this the same key as the reserved segment");
        assert_eq!(err.code(), tonic::Code::InvalidArgument, "{sent}");
        assert!(
            err.message().contains(sent),
            "a refusal names the value the caller sent rather than the constant (ADR-0569): {}",
            err.message()
        );
    }
    assert!(w.list("").await.is_empty(), "nothing was created");

    // AND THE CLASS THE SEGMENT PROTECTS IS STILL UNTOUCHED IN THAT SPELLING.
    // A guard that folded case into a PREFIX match would pass every assertion
    // above and delete the private class outright.
    w.try_register("LOCAL/home/max/src/alpha", "")
        .await
        .expect("a project beneath the reserved segment IS the private class, in any spelling");
    w.try_register("LOCALS", "a different organisation entirely")
        .await
        .expect("`LOCALS` merely shares five characters; a prefix is not a segment");

    // THE SLOT ALREADY TAKEN. `ALREADY_EXISTS` here would mean the guard let the
    // call through and the ENGINE stopped it, not this service.
    w.seed_project("local", "a-stranger").await;
    let err = w
        .try_register("LOCAL", "")
        .await
        .expect_err("the reserved segment is refused whatever its case");
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "ALREADY_EXISTS here means the unique index refused it rather than this service: {}",
        err.message()
    );
}

/// **THE REFUSAL HAPPENS INSIDE THE TRANSACTION THAT CARRIES THE CLAIM, so the
/// key is never spent** — which is the property `src/write.rs` states for the two
/// held-back verbs and takes here for the same reason: a key spent on an
/// operation nobody performed would make the caller's later retry of a DIFFERENT
/// request under it fail as a differing payload.
///
/// **WHAT THIS TEST CANNOT PIN, stated so that nobody reads more into it.** It
/// does not pin the ORDER of `refuse_reserved_root` against `idem::claim`.
/// Moving the guard BELOW the claim leaves this green, because the claim is
/// written inside the transaction and every pre-commit refusal rolls it back.
/// The invariant is real and it is guaranteed by TRANSACTION ATOMICITY rather
/// than by statement order; the guard sits above the claim because a refusal
/// should not open a transaction at all, which is a cost argument and not this
/// assertion.
///
/// The assertion is on the LEDGER rather than on a second call, and that is
/// forced rather than chosen: a claim is keyed by `(project_id, user_id, key)`
/// and the fixture stamps `scope.project_id` with the path under test, so a
/// retry under another path would be a different row and would pass whatever
/// this rpc did. Counting the rows for `local` is what actually distinguishes
/// the two orderings.
#[tokio::test]
async fn the_reserved_root_is_refused_before_the_idempotency_key_is_spent() {
    let w = world("project_db_registry_reserved_root_idem").await;

    let err = w
        .register_as("local", "", U1, Some("k-reserved"))
        .await
        .expect_err("the reserved segment is refused");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    let claims: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_write WHERE project_id = ?")
        .bind("local")
        .fetch_one(&w.pool)
        .await
        .expect("count the ledger");
    assert_eq!(
        claims, 0,
        "the refusal must not have reached the ledger: a key spent on an operation nobody \
         performed refuses the caller's next request under it as a differing payload"
    );
}
