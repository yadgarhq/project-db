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

/// **`exact` IS A STATEMENT ABOUT WHICH ROW WAS FOUND, AND THE STORE DECIDES
/// WHICH ROW.** `project.path` is `utf8mb4` with no `COLLATE`, so it takes
/// `utf8mb4_uca1400_ai_ci` and `uq_project_path` holds ONE slot for every
/// ASCII-case spelling of a path — `p.path IN (…)` therefore matches the row on
/// the shouted spelling. A byte comparison of the row's path against the
/// caller's then reports `exact: false` on a row the engine matched EXACTLY,
/// which is a dead end rather than a cosmetic wrong answer: the caller surfaces
/// `exact: false` as a D39 notice naming the id to register, and
/// `RegisterProject` answers `ALREADY_EXISTS` on the same unique index. A loop
/// with no exit.
///
/// **THE LOAD-BEARING ASSERTION RELATES THE TWO SPELLINGS TO EACH OTHER**
/// rather than each to a constant. They are one key in the store, so whatever
/// `exact` means it must mean the same thing for both.
#[tokio::test]
async fn every_ascii_case_spelling_of_a_registered_path_is_one_key_and_one_answer() {
    let w = world("project_db_resolve_exact_case").await;
    w.register(A).await;

    let shouted = A.to_ascii_uppercase();
    let registered = w.resolve(A).await.expect("the registered spelling");
    let other = w.resolve(&shouted).await.expect("the shouted spelling");

    assert_eq!(
        other.resolved_path, A,
        "the answer is the REGISTERED spelling, which is what every other record in the estate \
         is stamped with. Echoing {shouted:?} back would hand the caller a partition key that \
         matches no other record's"
    );
    assert_eq!(
        other.exact, registered.exact,
        "{shouted:?} and {A:?} are ONE row under `uq_project_path`, so the two resolutions \
         cannot disagree about whether the candidate had a row of its own"
    );
    assert!(
        other.exact,
        "and the value they agree on is true: a row was found ON the candidate, not on an \
         ancestor of it"
    );
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

/// `via_alias` IS THE SECOND HALF OF THE SAME COMPARISON — it is gated on
/// `exact`, so a byte comparison there takes this answer down with it. The alias
/// column folds case for exactly the reason the live path does: `alias_path` is
/// `utf8mb4` with no `COLLATE` too, and it is the PRIMARY KEY of its table.
///
/// Its own fixture, because a live path and an alias are matched by different
/// arms of the union and one arm being right proves nothing about the other.
#[tokio::test]
async fn a_differently_cased_former_path_still_reports_that_it_matched_an_alias() {
    let w = world("project_db_resolve_alias_case").await;
    let id = w.register(B).await;
    w.seed_alias(A, &id).await;

    let r = w
        .resolve(&A.to_ascii_uppercase())
        .await
        .expect("the shouted former path");

    assert_eq!(r.resolved_path, B, "the alias resolves to the live path");
    assert!(
        r.exact,
        "the candidate had a row of its own, in the alias table"
    );
    assert!(
        r.via_alias,
        "the contract sets this when candidate_path matched an alias rather than a live path, \
         and the store says this candidate did"
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

/// **THE RESOLUTION HAZARD THE WRITE-SIDE GUARD DOES NOT REACH.** `register`
/// refuses `local` now, and a code-only guard does not delete a row an earlier
/// build already accepted — so the store may HOLD one. If the walk still
/// consulted it, every private-class path in the estate would resolve into one
/// stranger's partition key with `exact: false`, and nothing would report it.
///
/// The row is seeded rather than registered for exactly that reason: it is the
/// state of a real database, not a contrivance.
#[tokio::test]
async fn a_private_path_never_resolves_up_into_a_squatted_reserved_root() {
    let w = world("project_db_resolve_reserved_squatted").await;
    w.seed_project("local", "a-stranger").await;

    let err = w
        .resolve("local/home/max/src/alpha")
        .await
        .expect_err("the reserved segment is not an ancestor of anything, row or no row");
    assert_eq!(err.code(), tonic::Code::NotFound);

    // THE CONTROL, and it is what stops this test passing for the wrong reason.
    // A guard that refused every path under the reserved segment would satisfy
    // the assertion above and break the private class outright; here the walk
    // still finds a real ancestor with the squatted row present, so what the
    // filter removed is one segment rather than the subtree.
    w.register("local/home/max/src/alpha").await;
    let r = w
        .resolve("local/home/max/src/alpha/svc")
        .await
        .expect("a registered private project is still an ancestor");
    assert_eq!(r.resolved_path, "local/home/max/src/alpha");
    assert!(!r.exact);
}

/// **THE SAME LINE COVERS THE ALIAS ARM, and it has to.** `ResolveProject` reads
/// `project_alias` in the second half of one union, bound from the same ancestor
/// list — so a former path at `local` is a second way into the identical
/// failure. Dropping the segment from the chain closes both; a guard written
/// against live paths alone would leave this open and no test would say so.
#[tokio::test]
async fn a_private_path_never_resolves_up_through_an_alias_at_the_reserved_root() {
    let w = world("project_db_resolve_reserved_alias").await;
    let id = w.register(B).await;
    w.seed_alias("local", &id).await;

    let err = w
        .resolve("local/home/max/src/alpha")
        .await
        .expect_err("a former path at the reserved segment is not an ancestor either");
    assert_eq!(err.code(), tonic::Code::NotFound);
}

/// **THE CANDIDATE THAT WOULD EMPTY THE CHAIN.** The reserved segment is
/// filtered out of the ancestor walk, so the bare segment would filter down to
/// nothing — and an empty `IN ()` is a syntax error the caller receives as
/// `INTERNAL "storage error"`. It is refused ahead of the walk instead, with the
/// reason in it.
#[tokio::test]
async fn the_bare_reserved_root_is_refused_as_a_candidate_rather_than_failing_inside() {
    let w = world("project_db_resolve_reserved_bare").await;
    w.register(ROOT).await;

    let err = w
        .resolve("local")
        .await
        .expect_err("the reserved segment names no project");
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "an empty ancestor list must not reach the engine as `IN ()`: {}",
        err.message()
    );
    assert!(err.message().contains("local"), "{}", err.message());
}

/// **THE READ-SIDE FILTER HAS TO FOLD CASE FOR THE SAME REASON THE REFUSAL
/// DOES, and the write-side guard cannot reach this at all: the row is already
/// there.** The ancestor walk is evaluated by the ENGINE, whose `p.path IN (…)`
/// compares under `utf8mb4_uca1400_ai_ci` — measured on a live MariaDB 11.8, and
/// the collation `project.path` takes because `src/schema.rs` declares no
/// `COLLATE`. So a chain still carrying `LOCAL` matches a row spelled `local`.
/// With a byte-comparing filter nothing is dropped from
/// `LOCAL/home/max/src/alpha`'s chain, the union's first arm finds the stranger's
/// row, and every private path spelled with a capital resolves into it with
/// `exact: false`.
#[tokio::test]
async fn an_upper_case_private_path_never_resolves_up_into_a_squatted_reserved_root() {
    let w = world("project_db_resolve_reserved_squatted_case").await;
    w.seed_project("local", "a-stranger").await;

    let err = w
        .resolve("LOCAL/home/max/src/alpha")
        .await
        .expect_err("the reserved segment is not an ancestor in any spelling, row or no row");
    assert_eq!(err.code(), tonic::Code::NotFound);

    // THE CONTROL. A filter that dropped the whole SUBTREE rather than the one
    // segment would satisfy the assertion above and break the private class; here
    // a real ancestor is still found with the squatted row present.
    w.register("local/home/max/src/alpha").await;
    let r = w
        .resolve("local/home/max/src/alpha/svc")
        .await
        .expect("a registered private project is still an ancestor");
    assert_eq!(r.resolved_path, "local/home/max/src/alpha");
    assert!(!r.exact);
}

/// **THE TWO COMPARISONS MUST BE THE SAME COMPARISON, and this is the test that
/// says so.** The bare segment is refused ahead of the walk precisely because the
/// filter below would otherwise reduce its chain to nothing, and `holes(0)`
/// renders `IN ()` — a syntax error the caller receives as `INTERNAL "storage
/// error"`. If a later edit makes the refusal byte-exact while the filter goes on
/// folding case, `LOCAL` passes the refusal, the filter removes it, and the chain
/// is empty. Asserting the refusal on a mixed-case spelling is what keeps the
/// two halves in step.
#[tokio::test]
async fn the_bare_reserved_root_in_upper_case_is_refused_rather_than_emptying_the_chain() {
    let w = world("project_db_resolve_reserved_bare_case").await;
    w.register(ROOT).await;

    let err = w
        .resolve("LOCAL")
        .await
        .expect_err("the reserved segment names no project in any spelling");
    assert_eq!(
        err.code(),
        tonic::Code::InvalidArgument,
        "an empty ancestor list must not reach the engine as `IN ()`: {}",
        err.message()
    );
    assert!(
        err.message().contains("LOCAL"),
        "a refusal names the value the caller sent (ADR-0569): {}",
        err.message()
    );
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

/// The repository that governs [`A`]'s namespace in the tests below.
///
/// **NEITHER A FIXTURE PATH NOR [`FIXTURE_REPO`], and both exclusions are load
/// bearing.** Not a path, because the defect this field closes was `source_repo`
/// BEING the row's own path — so a value derivable from `A` or `A_DEEP` would let
/// a resolver returning `resolved_path` satisfy the assertion. Not the harness
/// default either, because `World::register` fills the field from
/// `default_source_repo`: a test asserting the default cannot distinguish "the
/// value the registration carried" from "the value every org row in the fixture
/// carries".
const GOVERNING_REPO: &str = "pangolin-7c21/estate";

/// **THE TEST THE WHOLE CHANGE EXISTS FOR.** An unregistered org path resolves
/// upward to its nearest registered ancestor, and the ANCESTOR'S repository is
/// what comes back.
///
/// That is the value the gateway composes `PROJECT_UNREGISTERED_ORG`'s remediation
/// from — "open a pull request against `<source_repo>`" — and the candidate cannot
/// supply it, because the candidate is the path that is registered NOWHERE. The
/// row that governs it is the ancestor (D52 as amended by D53), so the ancestor's
/// column is the answer, and `resolve` binds it from the row the walk selected
/// rather than from `candidate_path`.
///
/// **THE MUTATION THAT REDS IT:** bind `req.candidate_path.clone()` into
/// `source_repo` in `src/read.rs` instead of the row's column. Every other
/// assertion in this file stays green, because no other one reads the field.
#[tokio::test]
async fn a_descendant_resolves_to_the_ancestors_repository_and_never_to_its_own_path() {
    let w = world("project_db_resolve_source_repo_ancestor").await;
    w.register_with_repo(A, "Alpha", GOVERNING_REPO)
        .await
        .expect("an org registration naming the repository that governs it");

    // A_DEEP is INSIDE A and is registered nowhere, which is the case the field
    // exists for: a caller working in a directory nobody has registered yet.
    let r = w.resolve(A_DEEP).await.expect("resolve");
    assert!(!r.exact, "A_DEEP has no row of its own; A is its ancestor");
    assert_eq!(r.resolved_path, A);
    assert_eq!(
        r.source_repo, GOVERNING_REPO,
        "the RESOLVED row's repository. The gateway names it in the remediation for a path that \
         is registered nowhere, so it cannot be derived from the candidate"
    );
    assert_ne!(
        r.source_repo, A_DEEP,
        "the candidate's own path is what a resolver reading the wrong value would answer"
    );

    // AND THE EXACT CASE TOO, so the assertion above is not satisfied by a
    // resolver that always answers the ancestor's column and never the row's.
    let r = w.resolve(A).await.expect("resolve");
    assert!(r.exact);
    assert_eq!(
        r.source_repo, GOVERNING_REPO,
        "the row found is its own resolved row when the match is exact"
    );
}

/// **A PRIVATE-CLASS ROW ANSWERS `""`, WHICH THE CONTRACT DEFINES AS ABSENT.**
///
/// `ck_project_class` gives every `local/...` row an `owner_user_id` and NULL
/// `source_repo` — a private project is governed by one account, not by a pull
/// request flow. proto3 gives a bare `string` no presence bit, so `NULL` has
/// exactly one spelling on the wire, and the field comment in
/// `yadgar/project/v1` states the other side of it: a consumer reads empty as
/// absent and must not interpolate it into prose.
///
/// **SO THE ASSERTION IS ON EMPTINESS AND NOT ON A SENTINEL.** A sentinel would be
/// a value a consumer could print — the gateway would compose "open a pull request
/// against `<none>`" — which is the class of wrong remediation this whole change
/// removes. The rpc must still SUCCEED: an absent repository is a legitimate
/// answer about a private project and never an error.
///
/// **THE MUTATION THAT REDS IT:** decode the column as `String` rather than
/// `Option<String>` in `ancestor_rows`. Then this resolve fails at DECODE and the
/// `expect` panics, which is also the reason the type is what it is.
#[tokio::test]
async fn a_private_class_row_resolves_with_an_empty_repository_rather_than_a_sentinel() {
    let w = world("project_db_resolve_source_repo_private").await;
    const PRIVATE: &str = "local/jaguar-4f80/thing";
    w.register(PRIVATE).await;

    let r = w.resolve(PRIVATE).await.expect(
        "a private project resolves; having no source repository is an ANSWER, not a failure",
    );
    assert!(r.exact);
    assert_eq!(
        r.source_repo, "",
        "empty IS absent on the wire, and a private row has no repository by `ck_project_class`"
    );

    // AND A DESCENDANT OF IT, because the ancestor walk is the path that carries
    // the column and NULL travels it too.
    let r = w
        .resolve(&format!("{PRIVATE}/deeper"))
        .await
        .expect("resolve");
    assert!(!r.exact);
    assert_eq!(r.resolved_path, PRIVATE);
    assert_eq!(
        r.source_repo, "",
        "a NULL column survives the ancestor walk"
    );
}

/// **THE SECOND UNION ARM, WHICH IS THE ONE AN EDIT MISSES.** `ancestor_rows`
/// selects the live path in one arm and `project_alias` in the other, and a change
/// made to one of them compiles.
///
/// An alias-matched row must answer with the LIVE row's repository: the alias is a
/// former path of that project (D53), the project is what is governed, and the
/// repository belongs to the project rather than to the spelling that found it.
/// `project_alias` holds no such column and must never grow one — a second copy is
/// the two-facts-that-drift shape ADR-0569 rejects — so the value comes off the
/// joined `project` row in both arms.
///
/// **THE MUTATION THAT REDS IT, AND WHY THE OBVIOUS ONE DOES NOT.** Deleting
/// `p.source_repo` from the second arm alone leaves the two arms with DIFFERENT
/// COLUMN COUNTS, which MariaDB refuses at prepare time — `sql::internal` maps
/// that to `INTERNAL` and every test in this file goes red, including the ancestor
/// one above, so it proves nothing about which arm this test reaches. The mutation
/// that discriminates keeps the arity and changes the value: select `NULL` in place
/// of `p.source_repo` in the ALIAS arm only. Measured on `mariadb:11.8.9` — this
/// test goes red and
/// `a_descendant_resolves_to_the_ancestors_repository_and_never_to_its_own_path`
/// stays green.
#[tokio::test]
async fn an_alias_matched_resolve_answers_with_the_live_rows_repository() {
    let w = world("project_db_resolve_source_repo_alias").await;
    let id = w
        .register_with_repo(B, "Bravo", GOVERNING_REPO)
        .await
        .expect("register")
        .meta
        .expect("meta")
        .id;
    // Seeded: `RenameProject` is held back in this release, and the alias read
    // paths are contract obligations regardless — see `World::seed_alias`.
    w.seed_alias(A, &id).await;

    // A IS THE CANDIDATE AND IT HAS NO LIVE ROW, so only the alias arm can match
    // it. That is what makes this test reach the arm rather than merely cover a
    // path the live arm would have answered anyway.
    let r = w.resolve(A).await.expect("a former path resolves");
    assert_eq!(r.resolved_path, B, "the alias resolves to the live path");
    assert!(r.via_alias, "the candidate matched an alias");
    assert_eq!(
        r.source_repo, GOVERNING_REPO,
        "the repository belongs to the PROJECT the alias names, not to the alias"
    );

    // AND THROUGH THE ALIAS AS AN ANCESTOR, which is the same arm reached by the
    // walk rather than by an exact match — the construction that strands a whole
    // subtree when the arm is wrong.
    let r = w
        .resolve(&format!("{A}/still-pointing-at-the-old-parent"))
        .await
        .expect("resolve");
    assert_eq!(r.resolved_path, B);
    assert!(!r.exact);
    assert_eq!(
        r.source_repo, GOVERNING_REPO,
        "a descendant of a FORMER parent path still names the repository that governs it"
    );
}
