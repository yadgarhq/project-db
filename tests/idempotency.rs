//! D9 against a real engine: a repeated key is a REPLAY, and a repeated key
//! carrying a DIFFERENT request is a REFUSAL.

mod support;
use support::*;

/// A key nothing in the implementation could produce.
const KEY: &str = "okapi-3f19-idem";

async fn world(name: &str) -> World {
    World::fresh(name).await
}

#[tokio::test]
async fn a_repeated_key_returns_the_first_outcome_and_writes_nothing_twice() {
    let w = world("project_db_idem_replay").await;

    let first = w
        .register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect("register");
    let replay = w
        .register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect("a retry must replay rather than fail");

    assert_eq!(
        first.meta.as_ref().map(|m| m.id.clone()),
        replay.meta.as_ref().map(|m| m.id.clone()),
        "the replay returns the FIRST attempt's identity"
    );
    assert_eq!(
        w.list("").await.len(),
        1,
        "the second delivery must not have registered a second project"
    );
}

/// **THE FAILURE MODE D9's AMENDMENT EXISTS FOR.** Replaying a differing request
/// hands the first request's outcome to a caller who sent a second: the
/// operation actually asked for is discarded and the answer reports success.
#[tokio::test]
async fn a_repeated_key_carrying_a_different_request_is_refused() {
    let w = world("project_db_idem_differing").await;
    w.register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect("register");

    let err = w
        .register_as(A, "a different display name entirely", U1, Some(KEY))
        .await
        .expect_err("a replay would discard what this request asked for");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // The FIRST request's outcome is what stands.
    assert_eq!(w.get(A).await.expect("get").display_name, "Alpha");
}

/// Decoding a stored `RenameProjectResponse` as a `RegisterProjectResponse`
/// would succeed and mean nothing, so a key already spent on another operation
/// is refused rather than guessed at.
///
/// **THE CLAIM IS SEEDED, BECAUSE ONE RPC IS ALL THAT CLAIMS IN THIS RELEASE.**
/// `RenameProject` and `ArchiveProject` refuse before they reach the ledger, so
/// `RegisterProject` is the only verb that writes a `project_write` row — which
/// would make `replay`'s rpc check unreachable through the service, and an
/// unreachable check is one nothing proves. Writing the row the way an earlier
/// or later release writes it is what keeps the branch honest. The ledger is
/// durable across releases, so this is a state a real store WILL hold.
#[tokio::test]
async fn one_key_reused_for_a_different_operation_is_refused() {
    let w = world("project_db_idem_other_rpc").await;
    w.seed_write_claim(A, U1, KEY, "RenameProject").await;

    let err = w
        .register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect_err("a key belongs to one operation");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(w.list("").await.is_empty(), "nothing was registered");
}

/// The key is CLIENT-supplied, so two clients will eventually choose the same
/// string. Deduplicating across users would hand one of them the other's record.
#[tokio::test]
async fn two_users_may_choose_the_same_key() {
    let w = world("project_db_idem_per_user").await;
    w.register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect("register");
    w.register_as(B, "Bravo", U2, Some(KEY))
        .await
        .expect("a second user's identical key is not a replay of the first's");

    assert_eq!(w.list("").await.len(), 2);
}

/// An absent or empty key means the caller does not participate — which must not
/// collapse every keyless write into one deduplicated slot.
#[tokio::test]
async fn a_keyless_write_is_not_deduplicated_against_other_keyless_writes() {
    let w = world("project_db_idem_keyless").await;
    w.try_register(A, "Alpha").await.expect("register");
    w.try_register(B, "Bravo")
        .await
        .expect("a second keyless write is a second write");

    assert_eq!(w.list("").await.len(), 2);
}

/// A write that FAILS leaves no claim, which is what keeps its retry a fresh
/// attempt rather than a replay of something that never happened.
#[tokio::test]
async fn a_failed_write_leaves_no_claim_behind() {
    let w = world("project_db_idem_failed").await;
    w.register(A).await;

    // Refused: A is already registered.
    let err = w
        .register_as(A, "second claim", U1, Some(KEY))
        .await
        .expect_err("already exists");
    assert_eq!(err.code(), tonic::Code::AlreadyExists);

    // THE SAME KEY, now for a request that can succeed. Had the failed attempt
    // committed its claim, this would replay a response that was never produced.
    w.register_as(B, "Bravo", U1, Some(KEY))
        .await
        .expect("the key is free because nothing was recorded under it");
    assert_eq!(w.list("").await.len(), 2);
}

/// KNOWN-ANSWER, ON THE STORAGE ENCODING. `src/idem.rs` pins the digest of a
/// byte string against published vectors; this pins that the thirty-two bytes
/// which reach the column are that digest of the REQUEST — a digest of some
/// other width, silently padded out by `BINARY(32)`, is invisible to an
/// assertion that stops at a function's return value.
#[tokio::test]
async fn the_stored_fingerprint_is_the_digest_of_the_request_without_its_scope() {
    use prost::Message as _;
    use sha2::{Digest, Sha256};
    use yadgar_project_db::pb::yadgar::project::v1::RegisterProjectRequest;

    let w = world("project_db_idem_fingerprint").await;
    w.register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect("register");

    // The payload as this module defines it: every field EXCEPT `scope` and
    // `idempotency`. Built here from the contract's own type rather than copied
    // out of the implementation, so a change to what is excluded fails here.
    let canonical = RegisterProjectRequest {
        idempotency: None,
        scope: None,
        path: A.into(),
        display_name: "Alpha".into(),
    };
    let expected: [u8; 32] = Sha256::digest(canonical.encode_to_vec()).into();

    assert_eq!(
        w.stored_fingerprint(A, U1, KEY).await,
        expected.to_vec(),
        "the column holds the digest of the request the caller sent, minus the two fields the \
         claim is keyed by"
    );
}

/// **A HELD-BACK VERB CLAIMS NOTHING**, because it opens no transaction at all
/// — so the key it was handed is still free for whatever the caller asks next.
/// Spend it and that next request, whatever it asked for, comes back refused as
/// a differing payload.
///
/// **WHAT THIS DOES NOT PIN, and it used to claim it did.** It is not evidence
/// that refusing BEFORE the claim is what saves the key. `register` refuses
/// after its claim twice over and saves it just the same, by rolling the
/// transaction back — see `a_failed_write_leaves_no_claim_behind` above, which
/// is the assertion for that half. Both are transaction atomicity; only the
/// second one needs it.
#[tokio::test]
async fn a_held_back_verb_does_not_spend_the_idempotency_key_it_was_given() {
    let w = world("project_db_idem_held_back").await;

    let err = w
        .rename_with(A, B, 1, Some(KEY))
        .await
        .expect_err("rename is held back");
    assert_eq!(err.code(), tonic::Code::Unimplemented);

    // THE SAME KEY, now for a request that can succeed.
    w.register_as(A, "Alpha", U1, Some(KEY))
        .await
        .expect("the key is free because the refusal claimed nothing");
    assert_eq!(w.list("").await.len(), 1);
}
