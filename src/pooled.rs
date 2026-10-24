//! One pooled connection per rpc, taken within the caller's deadline.
//!
//! **THE ACQUIRE IS BOUNDED BY WHAT IS LEFT OF THE CALLER'S DEADLINE**
//! (ADR-1019, yaadgaar #337). The chart's `database.acquireTimeoutSeconds` is
//! the CEILING, and the pool already carries it as its own `acquire_timeout`,
//! so it is read from there rather than threaded through a second time. The
//! effective bound is min(ceiling, remaining - `DEADLINE_MARGIN`): the margin
//! is what lets this store answer `DEADLINE_EXCEEDED` itself, and record it,
//! before tonic's server drops the handler at the caller's deadline. A
//! request with no deadline waits the ceiling, as before, and an exhausted
//! pool is still `UNAVAILABLE` there.
//!
//! **A `BEGIN` THAT NEVER RETURNED CLOSES ITS CONNECTION.** See
//! [`Pooled::begin`].

use std::time::Duration;

use sqlx::pool::PoolConnection;
use sqlx::{MySql, MySqlConnection, MySqlPool, Transaction};
use tonic::Status;
use yadgar_dial::Deadline;

use crate::sql::internal;

/// A connection taken out of the pool for one rpc.
pub struct Pooled {
    conn: PoolConnection<MySql>,
    /// Set while a `BEGIN` is in flight. A `Pooled` dropped with it set is
    /// closed instead of returned — see [`Pooled::begin`].
    armed: bool,
}

impl Pooled {
    /// Take a connection, within `deadline`.
    ///
    /// # Errors
    ///
    /// `DEADLINE_EXCEEDED` when the caller's time runs out first, without
    /// touching the pool when none is left; `UNAVAILABLE` when the pool's own
    /// ceiling does (see [`internal`]).
    pub async fn acquire(pool: &MySqlPool, deadline: Deadline) -> Result<Self, Status> {
        let ceiling = pool.options().get_acquire_timeout();
        let bound = deadline.bound(ceiling);
        if deadline.remaining().is_none() || bound >= ceiling {
            // THE CEILING DECIDES, so the pool's own timer is the only one
            // running: two timers set to one instant would race to pick the
            // status code.
            let conn = pool.acquire().await.map_err(internal)?;
            return Ok(Self::new(conn));
        }
        if bound.is_zero() {
            return Err(spent(bound));
        }
        match tokio::time::timeout(bound, pool.acquire()).await {
            Ok(conn) => Ok(Self::new(conn.map_err(internal)?)),
            Err(_) => Err(spent(bound)),
        }
    }

    fn new(conn: PoolConnection<MySql>) -> Self {
        Self { conn, armed: false }
    }

    /// Open a transaction on this connection.
    ///
    /// **CANCEL-SAFE, AND sqlx's OWN `begin` IS NOT.** Once callers send
    /// `grpc-timeout`, tonic's server drops a handler at the deadline wherever
    /// it is awaiting. sqlx counts a transaction only after `BEGIN` has
    /// RETURNED: `sqlx-mysql` 0.9.0 `MySqlTransactionManager::begin` bumps
    /// `transaction_depth` after `execute`, and `Transaction`'s `Drop` queues a
    /// `ROLLBACK` only when that depth is non-zero (`start_rollback`). So a
    /// `Transaction` dropped while `BEGIN` is in flight queues nothing, the
    /// release-time ping drains the reply, and the pool hands the next
    /// borrower a connection the server still holds inside a transaction —
    /// whose autocommit reads then run in that stale snapshot.
    /// `tests/deadline.rs::a_begin_dropped_in_flight_hands_the_next_borrower_a_clean_connection`
    /// measured it (`@@in_transaction` = 1). `Pool::begin` has the same window.
    ///
    /// So the connection is ARMED for the length of `BEGIN`: a `Pooled`
    /// dropped while armed calls `PoolConnection::close_on_drop`, and the pool
    /// opens a replacement instead of reusing it. A `BEGIN` that fails leaves it
    /// armed too, since what the server did with it is unknown. Once `BEGIN`
    /// returns, sqlx's own rollback-on-drop covers the rest of the transaction.
    ///
    /// # Errors
    ///
    /// What [`internal`] makes of a failed `BEGIN`.
    pub async fn begin(&mut self) -> Result<Transaction<'_, MySql>, Status> {
        let Self { conn, armed } = self;
        *armed = true;
        let tx = sqlx::Acquire::begin(&mut **conn).await.map_err(internal)?;
        *armed = false;
        Ok(tx)
    }

    /// The connection, for a read that needs no transaction.
    pub fn conn(&mut self) -> &mut MySqlConnection {
        &mut self.conn
    }
}

impl Drop for Pooled {
    fn drop(&mut self) {
        if self.armed {
            tracing::warn!(
                "a BEGIN was cut off before it returned; closing its connection rather than \
                 returning it to the pool inside a transaction"
            );
            self.conn.close_on_drop();
        }
    }
}

/// The refusal for a caller whose time ran out before a connection was free.
fn spent(bound: Duration) -> Status {
    tracing::warn!(
        ?bound,
        "project-db acquire bounded by the caller's deadline; the caller is giving up"
    );
    Status::deadline_exceeded(
        "no connection was free before the caller's deadline (grpc-timeout, ADR-1019)",
    )
}
