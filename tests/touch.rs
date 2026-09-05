//! `TouchProjects` — the debounced bookkeeping flush (D52), and the three
//! things it must not do.

mod support;
use support::*;

/// Sentinel seconds, deliberately not "now" and deliberately far from each
/// other. A test that used the current clock could not tell "the value the
/// caller sent" from "the value the engine substituted", which is the whole
/// question here.
const EARLIER: i64 = 1_700_000_000; // 2023-11-14T22:13:20Z
const LATER: i64 = 1_800_000_000; // 2027-01-15T08:00:00Z

async fn world(name: &str) -> World {
    World::fresh(name).await
}

#[tokio::test]
async fn a_touch_records_the_moment_the_caller_sent() {
    let w = world("project_db_touch_records").await;
    w.register(A).await;
    assert_eq!(w.stored_last_seen_at(A).await, None);

    w.touch(&[A], LATER).await.expect("touch");

    assert_eq!(
        w.stored_last_seen_at(A).await,
        Some(LATER),
        "the value is the caller's `seen_at`, never the server's clock — substituting the clock \
         would record when the flush ARRIVED, so a delayed flush would look like recent activity"
    );
    assert_eq!(
        w.get(A).await.expect("get").last_seen_at.map(|t| t.seconds),
        Some(LATER),
        "and it survives the trip back out through the UNIX_TIMESTAMP cast"
    );
}

/// **THE GUARD, AND IT IS PROVABLY LOAD-BEARING: delete the `last_seen_at <`
/// clause and this test fails.** Flushes are periodic and independent, so a
/// delayed one carrying an older `seen_at` arrives after a newer one. Its only
/// consumer is D47's notice expiry, which reasons in days — a project active all
/// week would age out because one late flush said it had not been.
#[tokio::test]
async fn a_late_flush_carrying_an_older_moment_does_not_move_the_value_backwards() {
    let w = world("project_db_touch_monotonic").await;
    w.register(A).await;

    w.touch(&[A], LATER).await.expect("the newer flush");
    w.touch(&[A], EARLIER).await.expect("the delayed one");

    assert_eq!(w.stored_last_seen_at(A).await, Some(LATER));
}

/// **THE TWO COLUMNS A BOOKKEEPING WRITE MUST NOT MOVE.** `version` is D8's
/// compare-and-set counter, so a background write bumping it would fail a
/// concurrent writer for a reason no caller could see — and it is held to that
/// NOW, while the verbs that move it are still held back, so the guarantee is
/// already true when they arrive rather than something to remember then.
/// `updated_at` means "when did this REGISTRATION last change", and an
/// `ON UPDATE CURRENT_TIMESTAMP` on the column would make every flush look like
/// an edit.
///
/// **THIS IS THE TEST THAT FAILS IF `ON UPDATE CURRENT_TIMESTAMP` IS ADDED BACK
/// TO THE MIGRATION.**
#[tokio::test]
async fn a_touch_moves_neither_the_version_nor_the_registrations_updated_at() {
    let w = world("project_db_touch_leaves_alone").await;
    w.register(A).await;
    let version = w.stored_version(A).await;
    // BACKDATED, and see `World::backdate_updated_at`: without it the mutant this
    // test exists to kill SURVIVES, because a TIMESTAMP holds whole seconds and
    // the registration and the flush land inside the same one. It is also what
    // proves the column can move at all, so the assertion below is not passing
    // merely because nothing in the process ever writes it.
    w.backdate_updated_at(A, EARLIER).await;
    assert_eq!(
        w.stored_updated_at(A).await,
        EARLIER,
        "the rig backdated the row"
    );

    w.touch(&[A], LATER).await.expect("touch");

    assert_eq!(
        w.stored_version(A).await,
        version,
        "D8's counter must not move"
    );
    assert_eq!(
        w.stored_updated_at(A).await,
        EARLIER,
        "a flush is not an edit of the registration"
    );
}

/// Records written before a rename still carry the old path, so a bucket keyed
/// on what a record carries holds FORMER paths.
#[tokio::test]
async fn a_former_path_touches_the_project_it_still_resolves_to() {
    let w = world("project_db_touch_alias").await;
    let id = w.register(B).await;
    // Seeded: `RenameProject` is held back in this release — see
    // `World::seed_alias`.
    w.seed_alias(A, &id).await;

    w.touch(&[A], LATER).await.expect("touch the former path");

    assert_eq!(
        w.stored_last_seen_at(B).await,
        Some(LATER),
        "a project used every day would otherwise look untouched since the day it was renamed"
    );
}

/// One unregistered path must not block the flush for every other project for
/// ever — but it must not vanish either.
#[tokio::test]
async fn an_unregistered_path_is_tolerated_and_the_registered_ones_still_land() {
    let w = world("project_db_touch_unknown").await;
    w.register(A).await;

    w.touch(&[A, &format!("{ROOT}/never-registered")], LATER)
        .await
        .expect("a flush is not refused over one stale bucket");

    assert_eq!(w.stored_last_seen_at(A).await, Some(LATER));
    // NOTHING WAS CREATED (D52).
    assert_eq!(w.list("").await.len(), 1);
}

#[tokio::test]
async fn the_same_path_twice_in_one_flush_is_one_path() {
    let w = world("project_db_touch_duplicates").await;
    w.register(A).await;
    w.touch(&[A, A, A], LATER).await.expect("touch");
    assert_eq!(w.stored_last_seen_at(A).await, Some(LATER));
}

#[tokio::test]
async fn a_flush_with_nothing_in_it_is_refused() {
    let w = world("project_db_touch_empty").await;
    w.register(A).await;

    let err = w.touch(&[], LATER).await.expect_err("an empty flush");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

/// **THE 2038 CLIFF, AND IT IS SILENT WITHOUT THIS CHECK.** `FROM_UNIXTIME`
/// answers NULL outside a `TIMESTAMP`'s range rather than failing, and the
/// column is nullable — so an out-of-range `seen_at` would ERASE the value
/// instead of advancing it, with nothing reporting anything.
#[tokio::test]
async fn a_moment_the_column_cannot_hold_is_refused_rather_than_stored_as_nothing() {
    let w = world("project_db_touch_range").await;
    w.register(A).await;
    w.touch(&[A], LATER).await.expect("a moment in range");

    // 0 IS IN THE LIST because a MariaDB TIMESTAMP begins at 1970-01-01 00:00:01,
    // so whether FROM_UNIXTIME(0) lands inside the column depends on the
    // session's time zone — a bound that holds only in UTC is not one.
    for out_of_range in [-1, 0, 2_147_483_648] {
        let err = w
            .touch(&[A], out_of_range)
            .await
            .expect_err("outside a TIMESTAMP's range");
        assert_eq!(err.code(), tonic::Code::InvalidArgument, "{out_of_range}");
    }

    assert_eq!(
        w.stored_last_seen_at(A).await,
        Some(LATER),
        "the refused flushes must not have erased the value that was there"
    );
}

#[tokio::test]
async fn a_flush_naming_no_moment_is_refused() {
    use tonic::Request;
    use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;
    use yadgar_project_db::pb::yadgar::project::v1::TouchProjectsRequest;

    let w = world("project_db_touch_no_seen_at").await;
    w.register(A).await;

    let err =
        w.db.touch_projects(Request::new(TouchProjectsRequest {
            scope: w.scope(ROOT, U1),
            paths: vec![A.to_string()],
            seen_at: None,
        }))
        .await
        .expect_err("substituting the server's clock would record the wrong moment");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(w.stored_last_seen_at(A).await, None);
}

#[tokio::test]
async fn a_path_that_is_not_a_project_path_refuses_the_flush() {
    let w = world("project_db_touch_bad_path").await;
    w.register(A).await;

    let err = w
        .touch(&[A, "not a path"], LATER)
        .await
        .expect_err("a bucket keyed on prose is a fault upstream");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(w.stored_last_seen_at(A).await, None);
}
