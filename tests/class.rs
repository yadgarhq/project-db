//! The project CLASS, and the two CHECK constraints that make it one fact.
//!
//! # What these tests are for
//!
//! `plans/project-validation.md` (ledger 881) adds three columns to `project` —
//! `source_repo` for the org class, `owner_user_id` for the private class, and
//! `visibility` — under two CHECKs:
//!
//! ```sql
//! CONSTRAINT ck_project_class
//!   CHECK ((source_repo IS NOT NULL) <> (owner_user_id IS NOT NULL))
//! CONSTRAINT ck_project_class_path
//!   CHECK ((owner_user_id IS NOT NULL) = (path LIKE 'local/%'))
//! ```
//!
//! The class is ONE fact — the path — and ADR-0605 made that structural: a
//! private project is `local/<account>/<path>` BY CONSTRUCTION OF THE ID, never
//! by a flag. The columns do not define the class; they carry what each class
//! NEEDS. The second CHECK is what stops them disagreeing with the path, which
//! is the two-facts-that-drift shape ADR-0569's rationale rejects. Without it a
//! row could hold an org path and an `owner_user_id`, and the class would depend
//! on which column a reader happened to inspect.
//!
//! # Why every one of these is an EXECUTED insert
//!
//! **A CHECK CAN BE DECORATIVE AND NOTHING IN THE DDL SAYS SO.** Whether the
//! deployed engine enforces CHECK at all is not something this repository can
//! read off its own migration — MySQL 5.7 parses CHECK and ignores it, and the
//! plan filed the deployed engine's behaviour as INFERRED for exactly that
//! reason. So each case below performs a real INSERT against the engine the
//! harness starts and asserts on the error the engine returns.
//!
//! **AND EVERY INSERT REACHES AROUND THE SERVICE.** `insert_class_row` writes
//! straight to the table on the pool, so no guard in `write.rs` can refuse first.
//! A test that drove `RegisterProject` would stay green with both CHECKs deleted,
//! because the Rust code would refuse and the assertion could not tell which
//! layer did it. The plan says so in as many words: "disable the service guard in
//! the test or drive SQL directly, otherwise the test passes while the CHECK is
//! decorative".
//!
//! **THE CONSTRAINT IS NAMED IN EVERY ASSERTION, never just "the insert failed".**
//! Several of these rows violate a constraint the test is not about — an org path
//! carrying both columns breaks both CHECKs at once — so an assertion that only
//! says "refused" cannot distinguish the two, and would pass for the wrong
//! reason. Each case below is shaped so exactly ONE of the two can fail, and
//! names it.

mod support;
use support::*;

use yadgar_project_db::pb::yadgar::common::v1::Visibility;

/// An org-class path under the fixture root. Not `local`-anything.
const ORG: &str = "pangolin-7c21/class-org";
/// A private-class path: `local/<account>/<path>` (ADR-0605).
const PRIVATE: &str = "local/jaguar-4f80/thing";
/// The same private path in a spelling the collation must fold.
const PRIVATE_SHOUTED: &str = "LOCAL/jaguar-4f80/thing";
/// An uppercase private path under a DIFFERENT account.
///
/// **IT CANNOT BE [`PRIVATE_SHOUTED`], and the reason is the property under test.**
/// `path` collates `utf8mb4_general_ci`, so `LOCAL/jaguar-4f80/thing` and
/// `local/jaguar-4f80/thing` are ONE key to `uq_project_path` — which is the fold
/// working. A test inserting both into one database meets `ERROR 1062` on the
/// second, from the unique index rather than from any CHECK. So the accepting case
/// uses a second account.
const PRIVATE_SHOUTED_OTHER: &str = "LOCAL/OCELOT-9b3/thing";

/// The repository an org row names as its PR target. A sentinel, so no assertion
/// can pass by accidentally matching a real path in the fixture set.
const REPO: &str = "pangolin-7c21/seed";

/// Assert that *err* is the engine refusing, and that it names *constraint*.
///
/// **THE FULL BACKTICKED NAME, because one is a PREFIX of the other.**
/// `ck_project_class` is the first sixteen characters of
/// `ck_project_class_path`, so a `contains("ck_project_class")` matches both and
/// would let either constraint satisfy either assertion. MariaDB's message is
/// ``CONSTRAINT `<name>` failed for `<db>`.`project` `` (error 4025), so the
/// backticks bound the name and the assertion is exact.
#[track_caller]
fn refused_by(err: &sqlx::Error, constraint: &str) {
    let sqlx::Error::Database(db) = err else {
        panic!("the engine did not refuse this row; sqlx reported {err:?}");
    };
    let wanted = format!("CONSTRAINT `{constraint}` failed");
    assert!(
        db.message().contains(&wanted),
        "expected the engine to name {wanted:?}; it said {:?}",
        db.message()
    );
}

/// NEITHER column set: the row belongs to no class at all.
///
/// `ck_project_class_path` is SATISFIED here — no owner and an org path agree —
/// so only exclusivity can fail, and the assertion can name it.
#[tokio::test]
async fn a_row_in_no_class_is_refused_by_the_engine() {
    let w = World::fresh("project_db_class_neither").await;
    let err = w
        .insert_class_row(ORG, None, None)
        .await
        .expect_err("a project belongs to a class; neither column set names none");
    refused_by(&err, "ck_project_class");
}

/// BOTH columns set: the row belongs to two classes.
///
/// **THE PATH IS THE PRIVATE ONE ON PURPOSE.** With an org path this row breaks
/// BOTH CHECKs and which one the engine reports is its own business; with
/// `local/...` the owner column agrees with the path, `ck_project_class_path`
/// passes, and exclusivity is the only thing left to fail.
#[tokio::test]
async fn a_row_in_two_classes_is_refused_by_the_engine() {
    let w = World::fresh("project_db_class_both").await;
    let err = w
        .insert_class_row(PRIVATE, Some(REPO), Some(U1))
        .await
        .expect_err("exactly one of the two columns is non-null");
    refused_by(&err, "ck_project_class");
}

/// An ORG path carrying an owner — the disagreement the second CHECK exists for.
///
/// Without it, this row is storable, and then `yadgarhq/docs` has an owner while
/// its path says it is organisational. Every reader picking a different column
/// gets a different answer about who may write there.
#[tokio::test]
async fn an_org_path_with_an_owner_is_refused_by_the_engine() {
    let w = World::fresh("project_db_class_org_owned").await;
    let err = w
        .insert_class_row(ORG, None, Some(U1))
        .await
        .expect_err("an owner belongs to the private class, and this path is not in it");
    refused_by(&err, "ck_project_class_path");
}

/// A PRIVATE path carrying a source repository — the same disagreement inverted.
///
/// This is the direction that matters most: a `local/` path registered as an ORG
/// project sits inside the reserved private root while answering to the PR flow,
/// so `PROJECT_UNREGISTERED_PERSONAL` and `PROJECT_UNREGISTERED_ORG` would both
/// be reachable for one path.
#[tokio::test]
async fn a_private_path_with_a_source_repo_is_refused_by_the_engine() {
    let w = World::fresh("project_db_class_private_repo").await;
    let err = w
        .insert_class_row(PRIVATE, Some(REPO), None)
        .await
        .expect_err("a `local/` path is the private class and takes an owner, not a repository");
    refused_by(&err, "ck_project_class_path");
}

/// **THE COLLATION TRIPWIRE, AND IT IS THE LOAD-BEARING ROW OF THIS FILE.**
///
/// `LIKE 'local/%'` is evaluated under `path`'s own collation. Migration 1 pins
/// that to `utf8mb4_general_ci`, which folds ASCII case and therefore AGREES with
/// `path::refuse_reserved_root`, whose comparison is
/// `eq_ignore_ascii_case`. Unpin it and the column takes `@@collation_server`,
/// which no deployment states.
///
/// **MEASURED, on `mariadb:11.8.9`, against this exact CHECK.** With `path`
/// collating `utf8mb4_bin` the engine ACCEPTS this row — `'LOCAL/...' LIKE
/// 'local/%'` is false, so a source repository agrees with it — and the store
/// then holds an ORG project inside the reserved private root, in a spelling
/// `refuse_reserved_root` treats as private and this CHECK treats as
/// organisational. That is the disagreement between the Rust fold and the engine
/// fold, and it is precisely what the pin removes.
///
/// So this test is not a case test. It is the assertion that the fold-forward
/// took: delete `COLLATE utf8mb4_general_ci` from migration 1 on an engine
/// defaulting to a binary collation and this insert succeeds, turning the
/// `expect_err` red.
#[tokio::test]
async fn an_uppercase_private_path_with_a_source_repo_is_refused_because_the_engine_folds_case() {
    let w = World::fresh("project_db_class_shouted_private").await;
    let err = w
        .insert_class_row(PRIVATE_SHOUTED, Some(REPO), None)
        .await
        .expect_err(
            "the store folds ASCII case, so every spelling of `local/` is the private class",
        );
    refused_by(&err, "ck_project_class_path");
}

/// **BOTH DIRECTIONS, or the CHECKs are proved only to refuse.** A constraint
/// that refuses everything satisfies every test above and breaks the module.
///
/// One legitimate row of each class, and the uppercase private spelling a third
/// time — this time the way a person would legitimately write it, to pin that the
/// fold admits as well as refuses.
#[tokio::test]
async fn a_legitimate_row_of_each_class_is_accepted() {
    let w = World::fresh("project_db_class_accepted").await;

    w.insert_class_row(ORG, Some(REPO), None)
        .await
        .expect("an org project names the repository whose PR flow governs it");
    w.insert_class_row(PRIVATE, None, Some(U1))
        .await
        .expect("a private project names its owner");
    w.insert_class_row(PRIVATE_SHOUTED_OTHER, None, Some(U2))
        .await
        .expect("`LOCAL/...` IS the private class, in any spelling — registry.rs registers one");

    // READ THE COLUMNS BACK. An insert that "succeeded" having silently stored
    // NULL in both would satisfy the line above and violate the constraint the
    // file is about.
    assert_eq!(
        w.stored_class(ORG).await,
        (Some(REPO.to_string()), None),
        "the org row"
    );
    assert_eq!(
        w.stored_class(PRIVATE).await,
        (None, Some(U1.to_string())),
        "the private row"
    );
}

/// `RegisterProject` still serves, and it fills the class columns from the PATH.
///
/// The contract carries no class fields — `RegisterProjectRequest` is `path` plus
/// `display_name` — so the service derives them, and this is where that
/// derivation is pinned. `visibility` is D12's default per class: `PRIVATE` for a
/// private project, `ORG` for an organisational one.
#[tokio::test]
async fn registration_fills_the_class_columns_from_the_path() {
    let w = World::fresh("project_db_class_register").await;

    w.try_register(ORG, "").await.expect("an org registration");
    assert_eq!(
        w.stored_class(ORG).await,
        (Some(ORG.to_string()), None),
        "an org registration carries a source repository and no owner"
    );
    assert_eq!(
        w.stored_visibility(ORG).await,
        Visibility::Org as i8,
        "D12's default for an organisational project"
    );

    w.try_register(PRIVATE, "")
        .await
        .expect("a private registration");
    assert_eq!(
        w.stored_class(PRIVATE).await,
        (None, Some(U1.to_string())),
        "a `local/` registration carries the calling user as owner and no repository"
    );
    assert_eq!(
        w.stored_visibility(PRIVATE).await,
        Visibility::Private as i8,
        "D12's default for a private project"
    );

    // VISIBILITY IS NEVER THE ENUM'S ZERO. `VISIBILITY_UNSPECIFIED = 0` is
    // documented as "never persisted; rejected at validation", and the column is
    // `NOT NULL` with no DEFAULT — so an insert path that forgot to bind it would
    // store 0 and nothing else here would notice.
    for path in [ORG, PRIVATE] {
        assert_ne!(
            w.stored_visibility(path).await,
            Visibility::Unspecified as i8,
            "{path} stored VISIBILITY_UNSPECIFIED, which is never a value"
        );
    }
}

/// **A TRIPWIRE, NOT A GUARANTEE: `source_repo` IS THE PATH ITSELF, AND GIVEN 3
/// OF THE PLAN NEEDS IT NOT TO BE.**
///
/// The plan's given 3 has the seed repository register the org ROOT carrying its
/// own address as `source_repo`, so that an unregistered org path resolves upward
/// to that row and the gateway composes "open a PR against `<root.source_repo>`"
/// FROM DATA. The root row's path is a single segment — `pangolin-7c21` here,
/// `yadgarhq` in the estate — and the repository governing it is one BENEATH it.
/// Those are different values by construction, and this stage cannot tell them
/// apart: `RegisterProjectRequest` has no `source_repo` field, and the plan's
/// stage 2 adds one only to `ResolveProjectResponse`, which is the read side.
///
/// So this test asserts the WRONG value on purpose, and says why. When the
/// register contract gains the field, this test is the thing that fails and
/// points at the sentence that has to change. The same limitation applies to a
/// marker-declared subpath: `yadgarhq/docs/plans` is governed by `yadgarhq/docs`,
/// and registering it here would claim itself.
#[tokio::test]
async fn a_single_segment_registration_names_itself_which_given_3_says_it_must_not() {
    let w = World::fresh("project_db_class_root_row").await;
    // `ROOT` is a single segment and not the reserved one, so it registers.
    w.try_register(ROOT, "the namespace anchor")
        .await
        .expect("a single-segment org path is registrable");
    assert_eq!(
        w.stored_class(ROOT).await,
        (Some(ROOT.to_string()), None),
        "STAGE 1 LIMITATION: the anchor names ITSELF as the repository whose PR flow governs \
         it, which is not a repository at all. Given 3 of plans/project-validation.md needs a \
         repository BENEATH the root here, and no rpc in the contract can carry one. When \
         RegisterProject gains a source_repo field, this assertion is what must change."
    );
}
