//! The caller's deadline bounds every pool acquire (ADR-1019, yaadgaar #337),
//! and a `BEGIN` cut off mid-flight never hands its connection back dirty.
//!
//! **THE POOL IS ONE CONNECTION, AND THE TEST HOLDS IT.** Every rpc that
//! reaches the store must then wait in `acquire`, which is the wait the
//! caller's deadline has to cut short. The acquire CEILING is the pool's own
//! `acquire_timeout` — the chart's `database.acquireTimeoutSeconds` — and is
//! set far above the caller's budget, so an answer inside the budget can only
//! have come from the deadline.

mod support;

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sqlx::mysql::MySqlPoolOptions;
use sqlx::MySqlPool;
use tonic::{Code, Request, Status};
use yadgar_dial::Deadline;
use yadgar_project_db::pb::yadgar::project::v1::project_db_service_server::ProjectDbService as _;
use yadgar_project_db::pb::yadgar::project::v1::*;
use yadgar_project_db::pooled::Pooled;
use yadgar_project_db::service::{ProjectDb, REGISTER_CALLER, SYNC_CALLER};

use support::*;

/// What the caller has left when its request arrives.
const BUDGET: Duration = Duration::from_millis(300);

/// The pool's own acquire ceiling, far above [`BUDGET`].
const CEILING: Duration = Duration::from_secs(10);

/// A one-connection pool on the fixture database `name`.
async fn one_connection(name: &str, acquire_timeout: Duration) -> MySqlPool {
    MySqlPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(acquire_timeout)
        // `yadgar-store`'s `pool_options` sets it, so the fixture does too.
        .test_before_acquire(true)
        .connect(&database_url(name))
        .await
        .expect("one-connection pool")
}

/// A request carrying `grpc-timeout`, as a caller's tonic client sends it.
fn with_budget<T>(message: T) -> Request<T> {
    let mut request = Request::new(message);
    request.metadata_mut().insert(
        "grpc-timeout",
        format!("{}m", BUDGET.as_millis()).parse().expect("header"),
    );
    request
}

/// Run `call` against a store whose only connection is held elsewhere, and
/// assert it answers DEADLINE_EXCEEDED inside [`BUDGET`].
async fn answers_within_budget<F, T>(rpc: &str, call: F)
where
    F: Future<Output = Result<T, Status>>,
    T: std::fmt::Debug,
{
    let started = Instant::now();
    // A GUARD ON THE TEST, not on the code: without the bound the rpc waits
    // out the ten-second ceiling, and the test says so in two.
    let answered = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{rpc} was still waiting for a connection 2s into a {BUDGET:?} budget: the \
                 acquire is bounded by the pool's ceiling, not by the caller's deadline"
            )
        });
    let elapsed = started.elapsed();
    let err = answered.expect_err(rpc);
    assert_eq!(err.code(), Code::DeadlineExceeded, "{rpc}: {err:?}");
    assert!(
        elapsed < BUDGET,
        "{rpc} answered after {elapsed:?}; it must answer before the caller's {BUDGET:?} runs out"
    );
}

#[tokio::test]
async fn every_rpc_that_reaches_the_store_answers_deadline_exceeded_inside_the_callers_budget() {
    let name = "pdb_deadline_bound";
    let w = World::fresh(name).await;
    let pool = one_connection(name, CEILING).await;
    let _held = pool.acquire().await.expect("hold the only connection");
    let db = ProjectDb::new(pool.clone());
    let scope = w.scope(ROOT, U1);

    answers_within_budget(
        "ResolveProject",
        db.resolve_project(with_budget(ResolveProjectRequest {
            scope: scope.clone(),
            candidate_path: A.into(),
        })),
    )
    .await;
    answers_within_budget(
        "GetProject",
        db.get_project(with_budget(GetProjectRequest {
            scope: scope.clone(),
            path: A.into(),
        })),
    )
    .await;
    answers_within_budget(
        "ListProjects",
        db.list_projects(with_budget(ListProjectsRequest {
            scope: scope.clone(),
            under_path: String::new(),
            status: None,
            page_size: 0,
            page_token: String::new(),
        })),
    )
    .await;
    answers_within_budget(
        "TouchProjects",
        db.touch_projects(with_budget(TouchProjectsRequest {
            scope: scope.clone(),
            paths: vec![A.into()],
            seen_at: Some(prost_types::Timestamp {
                seconds: 1_700_000_000,
                nanos: 0,
            }),
        })),
    )
    .await;
    // The two caller-gated writes, past their gate: the identity is the
    // parameter the handler fills from the client leaf.
    answers_within_budget(
        "RegisterProject",
        db.register_as(
            Some(REGISTER_CALLER),
            w.registration(&format!("local/{U1_SEG}/probe"), "", U1, None, ""),
            Deadline::after(BUDGET),
        ),
    )
    .await;
    answers_within_budget(
        "SyncProjects",
        db.sync_as(
            Some(SYNC_CALLER),
            w.sync_request(&[declared(ROOT, "", FIXTURE_REPO)], &[], false),
            Deadline::after(BUDGET),
        ),
    )
    .await;
}

/// NO DEADLINE FALLS BACK TO THE CEILING: the acquire still fails, and at the
/// pool's own `acquire_timeout`, as `UNAVAILABLE` — the retryable code
/// `sql::internal` gives an exhausted pool.
#[tokio::test]
async fn without_a_deadline_the_acquire_still_fails_at_the_pools_ceiling() {
    let name = "pdb_deadline_ceiling";
    let w = World::fresh(name).await;
    let ceiling = Duration::from_millis(400);
    let pool = one_connection(name, ceiling).await;
    let _held = pool.acquire().await.expect("hold the only connection");
    let db = ProjectDb::new(pool.clone());

    let started = Instant::now();
    let err = tokio::time::timeout(
        Duration::from_secs(5),
        db.get_project(Request::new(GetProjectRequest {
            scope: w.scope(A, U1),
            path: A.into(),
        })),
    )
    .await
    .expect("the ceiling must still end the wait")
    .expect_err("no connection is free");
    let elapsed = started.elapsed();
    assert_eq!(err.code(), Code::Unavailable, "{err:?}");
    assert!(
        elapsed >= ceiling,
        "answered after {elapsed:?}, before the {ceiling:?} ceiling: something other than the \
         pool's acquire timeout ended the wait"
    );
}

/// A deadline already spent never takes a connection at all.
#[tokio::test]
async fn a_spent_deadline_is_refused_without_waiting_for_a_connection() {
    let name = "pdb_deadline_spent";
    let _w = World::fresh(name).await;
    let pool = one_connection(name, CEILING).await;
    let _held = pool.acquire().await.expect("hold the only connection");

    let started = Instant::now();
    let err = Pooled::acquire(&pool, Deadline::after(Duration::ZERO))
        .await
        .err()
        .expect("nothing is left of the caller's time");
    assert_eq!(err.code(), Code::DeadlineExceeded, "{err:?}");
    assert!(started.elapsed() < Duration::from_millis(50));
}

/// **A `BEGIN` CUT OFF MID-FLIGHT MUST NOT HAND BACK AN OPEN TRANSACTION.**
///
/// Once callers send `grpc-timeout`, tonic's server drops a handler future at
/// the deadline wherever it is awaiting. sqlx counts a transaction only after
/// `BEGIN` has RETURNED (`sqlx-mysql` 0.9.0 `transaction.rs`: `transaction_depth`
/// is incremented after `execute`), so a `Transaction` dropped while `BEGIN` is
/// in flight queues no `ROLLBACK`, and the connection goes back to the pool
/// with the server still inside the transaction. The next borrower's
/// autocommit reads would then run inside that stale snapshot.
///
/// The cancellation is driven by hand: the `begin` future is polled ONCE, which
/// sends `BEGIN` and parks on the reply, and is then dropped.
#[tokio::test]
async fn a_begin_dropped_in_flight_hands_the_next_borrower_a_clean_connection() {
    let name = "pdb_deadline_begin";
    let _w = World::fresh(name).await;
    let pool = one_connection(name, CEILING).await;

    let default_isolation: String = {
        let mut conn = pool.acquire().await.expect("acquire");
        sqlx::query_scalar("SELECT @@session.tx_isolation")
            .fetch_one(&mut *conn)
            .await
            .expect("session default")
    };

    {
        let mut pooled = Pooled::acquire(&pool, Deadline::none())
            .await
            .expect("acquire");
        {
            let mut begin = pin!(pooled.begin());
            let mut cx = Context::from_waker(Waker::noop());
            assert!(
                matches!(begin.as_mut().poll(&mut cx), Poll::Pending),
                "BEGIN answered inside one poll, so this run cannot cut it off mid-flight"
            );
        } // the handler is dropped here, as tonic drops it at the deadline
    } // and the connection with it

    // THE ONLY CONNECTION, so this is the one the cancelled BEGIN used. The
    // pool returns it on a spawned task; the acquire waits for that.
    let mut conn = pool.acquire().await.expect("reacquire");
    let in_transaction: u64 = sqlx::query_scalar("SELECT @@in_transaction")
        .fetch_one(&mut *conn)
        .await
        .expect("in_transaction");
    assert_eq!(
        in_transaction, 0,
        "the next borrower was handed a connection still inside the cancelled BEGIN's \
         transaction"
    );

    let mut tx = sqlx::Acquire::begin(&mut *conn).await.expect("begin");
    let isolation: String = sqlx::query_scalar("SELECT @@tx_isolation")
        .fetch_one(&mut *tx)
        .await
        .expect("isolation");
    tx.rollback().await.expect("rollback");
    assert_eq!(
        isolation, default_isolation,
        "the next transaction must run at the session default"
    );
}
