//! `RegisterProject` admits a private path only under the REGISTRANT'S OWN
//! account segment (ADR-0954, which settles the rendering ADR-0605 left open;
//! ledger 641).
//!
//! The segment is the gateway-attested `Scope.user_id` with `yadgar:user:`
//! removed: the lowercase uuidv7. So `local/<seg>/...` registers for the user
//! whose id renders `<seg>`, and for nobody else.
//!
//! **THE SEGMENTS ARE LITERALS, NEVER DERIVED FROM THE IDS (ADR-0573).** A test
//! that stripped the prefix the way the code does would move with the code and
//! pin nothing. [`U1_SEG`] and [`U2_SEG`] are spelled out in the fixture.
//!
//! **CASE: REFUSED, NEVER NORMALISED.** The store folds ASCII case
//! (`uq_project_path`, `ck_project_class_path`), so `LOCAL/<seg>/x` and an
//! upper-case uuid are the same KEY as the canonical spelling. This rpc admits
//! only the canonical spelling — `local` and the lowercase uuid, byte for byte
//! — and refuses every other one rather than rewriting it, the rule
//! `src/path.rs` applies to the whole grammar ("refused, never normalised").

mod support;
use support::*;

use tonic::Code;

/// A version-4 uuid, canonical in every other respect. ADR-0954 names the
/// uuidv7 `iam-db` mints, so a v4 is not a user id this estate issues.
const V4: &str = "0192f3a4-5b6c-4d8e-9f01-23456789abc1";

async fn world(name: &str) -> World {
    World::fresh(name).await
}

/// The refusal for a path outside the registrant's own private root.
fn assert_foreign(err: &tonic::Status, what: &str) {
    assert_eq!(err.code(), Code::PermissionDenied, "{what}: {err:?}");
}

#[tokio::test]
async fn a_foreign_account_segment_is_refused_and_nothing_is_written() {
    let w = world("project_db_account_foreign").await;
    let foreign = format!("local/{U2_SEG}/notes");

    let err = w
        .register_as(&foreign, "", U1, None)
        .await
        .expect_err("U1 may not register under U2's account");
    assert_foreign(&err, "foreign segment");
    assert!(
        err.message().contains(U1_SEG),
        "the refusal names the caller's OWN segment so they can fix the path: {}",
        err.message()
    );
    assert_eq!(w.project_count().await, 0, "nothing was written");
}

/// **NO EXISTENCE ORACLE (ADR-0954).** The refusal for a foreign path must be
/// the same whether or not that path is registered. A guard below the INSERT
/// would answer `ALREADY_EXISTS` for a registered path and `PERMISSION_DENIED`
/// for an unregistered one — telling a stranger which private paths exist.
#[tokio::test]
async fn a_foreign_path_is_refused_identically_whether_or_not_it_is_registered() {
    let w = world("project_db_account_oracle").await;
    let theirs = format!("local/{U2_SEG}/notes");

    let before = w
        .register_as(&theirs, "", U1, Some("k-1"))
        .await
        .expect_err("unregistered foreign path");
    w.register_as(&theirs, "", U2, None)
        .await
        .expect("U2 registers its own path");
    let after = w
        .register_as(&theirs, "", U1, Some("k-1"))
        .await
        .expect_err("registered foreign path");

    assert_foreign(&before, "unregistered");
    assert_foreign(&after, "registered");
    assert_eq!(
        before.message(),
        after.message(),
        "the answer must not depend on whether the path exists"
    );
    assert_eq!(w.project_count().await, 1, "only U2's own row exists");
}

#[tokio::test]
async fn the_registrants_own_segment_is_accepted_and_owned_by_them() {
    let w = world("project_db_account_own").await;
    let own = format!("local/{U1_SEG}/home/max/git/alpha");
    let root = format!("local/{U1_SEG}");

    w.register_as(&own, "", U1, None)
        .await
        .expect("a registrant's own segment registers");
    // THE ACCOUNT ROOT ITSELF is a legitimate rollup of one's own tree (D53).
    w.register_as(&root, "", U1, None)
        .await
        .expect("the bare account root is the registrant's own too");

    assert_eq!(w.stored_class(&own).await, (None, Some(U1.to_string())));
    assert_eq!(w.stored_class(&root).await, (None, Some(U1.to_string())));
}

/// CASE VARIANTS: refused, never normalised — see the module header.
///
/// The `LOCAL/<U2_SEG>` row is the one that matters most: a guard that read
/// the account only beneath a byte-exact `local` would see no private path
/// here at all, while the engine files it in U2's private class.
#[tokio::test]
async fn every_non_canonical_spelling_of_a_private_root_is_refused() {
    let w = world("project_db_account_case").await;
    let upper_own = U1_SEG.to_ascii_uppercase();
    let upper_foreign = U2_SEG.to_ascii_uppercase();

    for path in [
        format!("local/{upper_own}/x"),
        format!("LOCAL/{U1_SEG}/x"),
        format!("Local/{U1_SEG}/x"),
        format!("LOCAL/{U2_SEG}/x"),
        format!("local/{upper_foreign}/x"),
    ] {
        let err = w.register_as(&path, "", U1, None).await.expect_err(&path);
        assert_foreign(&err, &path);
    }
    assert_eq!(w.project_count().await, 0, "nothing was written");
}

/// The user id copied into the path VERBATIM, prefix and all. Its colons are
/// not a path character, so the grammar refuses it before the account guard
/// is reached; a hyphenated look-alike reaches the guard and is foreign.
#[tokio::test]
async fn a_segment_carrying_the_user_id_prefix_is_refused() {
    let w = world("project_db_account_prefix").await;

    let err = w
        .register_as(&format!("local/{U1}/x"), "", U1, None)
        .await
        .expect_err("`yadgar:user:` is not part of the segment");
    assert_eq!(err.code(), Code::InvalidArgument, "{err:?}");

    let err = w
        .register_as(&format!("local/yadgar-user-{U1_SEG}/x"), "", U1, None)
        .await
        .expect_err("a look-alike of the prefix is not the segment either");
    assert_foreign(&err, "prefix look-alike");
    assert_eq!(w.project_count().await, 0, "nothing was written");
}

/// **A NON-CANONICAL `user_id` FAILS CLOSED, EVEN WHEN THE PATH NAMES IT.** Each
/// row pairs a bad user id with the path a naive strip-and-compare would accept
/// for it: an upper-case uuid "matches" an upper-case segment, a bare uuid
/// "matches" itself once nothing is stripped. None of them is a user id this
/// estate mints, so none of them owns a segment.
#[tokio::test]
async fn a_non_canonical_or_empty_user_id_registers_nothing() {
    let w = world("project_db_account_bad_user").await;
    let upper = U1_SEG.to_ascii_uppercase();
    let simple = U1_SEG.replace('-', "");

    let cases: Vec<(String, String)> = vec![
        (String::new(), format!("local/{U1_SEG}/x")),
        (U1_SEG.to_string(), format!("local/{U1_SEG}/x")),
        (format!("yadgar:user:{upper}"), format!("local/{upper}/x")),
        (format!("YADGAR:USER:{U1_SEG}"), format!("local/{U1_SEG}/x")),
        (
            format!("yadgar:user:{{{U1_SEG}}}"),
            format!("local/{U1_SEG}/x"),
        ),
        (format!("yadgar:user:{simple}"), format!("local/{simple}/x")),
        (
            format!("yadgar:user:urn:uuid:{U1_SEG}"),
            format!("local/{U1_SEG}/x"),
        ),
        (format!("yadgar:user:{V4}"), format!("local/{V4}/x")),
        (
            format!("yadgar:project:{U1_SEG}"),
            format!("local/{U1_SEG}/x"),
        ),
        (
            format!("yadgar:user:{U1_SEG} "),
            format!("local/{U1_SEG}/x"),
        ),
        ("yadgar:user:".to_string(), format!("local/{U1_SEG}/x")),
    ];
    for (user, path) in cases {
        let err = w
            .register_as(&path, "", &user, None)
            .await
            .expect_err(&format!("user_id {user:?} owns no segment"));
        assert_eq!(err.code(), Code::InvalidArgument, "{user:?}: {err:?}");
        assert!(
            err.message().contains("user_id"),
            "{user:?}: the refusal names the field: {}",
            err.message()
        );
    }
    assert_eq!(w.project_count().await, 0, "nothing was written");
}
