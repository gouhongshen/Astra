use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use sqlx::{
    Connection, MySql, MySqlConnection, Transaction, TransactionManager,
    mysql::MySqlTransactionManager, pool::PoolConnection,
};

mod sealed {
    pub trait TransactionConnection {}
}

/// A transaction view accepted by helpers that must execute while a
/// transaction is open.
///
/// The marker is sealed so a plain pooled connection cannot accidentally be
/// passed to a helper that relies on row locks surviving until commit. The
/// standard SQLx transaction and Astra's cancellation-safe owned transaction
/// are the only implementations.
pub trait TransactionConnection:
    std::ops::DerefMut<Target = MySqlConnection> + sealed::TransactionConnection
{
}

impl<'a> sealed::TransactionConnection for Transaction<'a, MySql> {}
impl<'a> TransactionConnection for Transaction<'a, MySql> {}

static POOL_ACQUIRE_ATTEMPTS: AtomicU64 = AtomicU64::new(0);
static POOL_ACQUIRE_FAILURES: AtomicU64 = AtomicU64::new(0);
static POOL_ACQUIRE_SLOW: AtomicU64 = AtomicU64::new(0);
static POOL_ACQUIRE_WAIT_MICROS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DbPoolAcquireTelemetrySnapshot {
    pub attempts: u64,
    pub failures: u64,
    pub slow: u64,
    pub wait_micros: u64,
}

pub fn db_pool_acquire_telemetry_snapshot() -> DbPoolAcquireTelemetrySnapshot {
    DbPoolAcquireTelemetrySnapshot {
        attempts: POOL_ACQUIRE_ATTEMPTS.load(Ordering::Relaxed),
        failures: POOL_ACQUIRE_FAILURES.load(Ordering::Relaxed),
        slow: POOL_ACQUIRE_SLOW.load(Ordering::Relaxed),
        wait_micros: POOL_ACQUIRE_WAIT_MICROS.load(Ordering::Relaxed),
    }
}

async fn acquire_pool_connection(
    pool: &sqlx::Pool<MySql>,
) -> Result<PoolConnection<MySql>, sqlx::Error> {
    POOL_ACQUIRE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let connection = pool.acquire().await;
    let elapsed = started.elapsed();
    let elapsed_micros = elapsed.as_micros().try_into().unwrap_or(u64::MAX);
    POOL_ACQUIRE_WAIT_MICROS.fetch_add(elapsed_micros, Ordering::Relaxed);
    if elapsed >= astra_core::MATRIXONE_SLOW_POOL_ACQUIRE_AFTER {
        POOL_ACQUIRE_SLOW.fetch_add(1, Ordering::Relaxed);
    }
    connection.inspect_err(|_| {
        POOL_ACQUIRE_FAILURES.fetch_add(1, Ordering::Relaxed);
    })
}

/// A checked-out shared-pool connection that is reusable only after its
/// caller explicitly proves the MySQL protocol is synchronized.
///
/// Dropping a future while SQLx is awaiting a MySQL response can otherwise
/// return a connection with an in-flight exchange to the pool. Normal paths
/// call [`Self::release`] after completing every query; timeout/cancellation
/// paths reach `Drop` and close only that physical connection.
pub struct CancellationSafePoolConnection {
    connection: Option<PoolConnection<MySql>>,
}

impl CancellationSafePoolConnection {
    pub async fn acquire(pool: &sqlx::Pool<MySql>) -> Result<Self, sqlx::Error> {
        let connection = acquire_pool_connection(pool).await?;
        Ok(Self {
            connection: Some(connection),
        })
    }

    pub fn connection_mut(&mut self) -> &mut sqlx::MySqlConnection {
        self.connection
            .as_deref_mut()
            .expect("cancellation-safe connection already released")
    }

    pub async fn begin(&mut self) -> Result<sqlx::Transaction<'_, MySql>, sqlx::Error> {
        self.connection_mut().begin().await
    }

    pub fn release(mut self) {
        drop(self.connection.take());
    }

    /// Remove this checkout from pool reuse without waiting for a graceful
    /// protocol close.
    ///
    /// Use this after a cancelled or timed-out MySQL exchange: the client no
    /// longer knows whether a response is still in flight, so returning the
    /// physical connection to the idle pool would be unsafe. `detach` restores
    /// the pool's logical capacity; immediately dropping the detached
    /// connection closes the socket instead of leaking a pool permit.
    pub(crate) fn discard(mut self) {
        if let Some(connection) = self.connection.take() {
            drop(connection.detach());
        }
    }
}

impl Drop for CancellationSafePoolConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_mut() {
            connection.close_on_drop();
        }
    }
}

/// A manually managed transaction whose physical checkout is never returned
/// to a shared pool while a MySQL exchange may still be in flight.
///
/// This is the owned form for code that needs to pass the transaction through
/// several helpers. `sqlx::Transaction` borrows a checkout, so it cannot carry
/// the close-on-cancel guard across those helpers without a second parallel
/// state machine. A successful commit returns the synchronized checkout;
/// every other drop closes it or starts a safe rollback after the last
/// completed exchange.
pub(crate) struct CancellationSafeTransaction {
    connection: Option<PoolConnection<MySql>>,
    transaction_open: bool,
    discard_on_drop: bool,
}

impl sealed::TransactionConnection for CancellationSafeTransaction {}
impl TransactionConnection for CancellationSafeTransaction {}

impl CancellationSafeTransaction {
    pub(crate) async fn begin(pool: &sqlx::Pool<MySql>) -> Result<Self, sqlx::Error> {
        let connection = acquire_pool_connection(pool).await?;
        let mut transaction = Self {
            connection: Some(connection),
            transaction_open: false,
            discard_on_drop: true,
        };
        MySqlTransactionManager::begin(transaction.connection_mut(), None).await?;
        transaction.transaction_open = true;
        Ok(transaction)
    }

    pub(crate) fn connection_mut(&mut self) -> &mut MySqlConnection {
        self.connection
            .as_deref_mut()
            .expect("cancellation-safe transaction connection already released")
    }

    pub(crate) fn mark_io_in_flight(&mut self) {
        self.discard_on_drop = true;
    }

    pub(crate) fn mark_io_complete(&mut self) {
        self.discard_on_drop = false;
    }

    pub(crate) async fn commit(mut self) -> Result<(), sqlx::Error> {
        if self.transaction_open {
            self.mark_io_in_flight();
            MySqlTransactionManager::commit(self.connection_mut()).await?;
            self.transaction_open = false;
        }
        self.discard_on_drop = false;
        self.connection.take();
        Ok(())
    }

    /// Roll back after the caller has received a completed, ordinary result.
    ///
    /// Cancellation never reaches this method: dropping the future drops the
    /// guard instead, and [`Drop`] closes the physical checkout. If rollback
    /// itself fails, the still-armed guard follows that same close-on-error
    /// path rather than returning an uncertain connection to the pool.
    pub(crate) async fn rollback(&mut self) -> Result<(), sqlx::Error> {
        if self.transaction_open {
            self.mark_io_in_flight();
            MySqlTransactionManager::rollback(self.connection_mut()).await?;
            self.transaction_open = false;
        }
        self.discard_on_drop = false;
        // A rolled-back transaction is terminal. Dropping the synchronized
        // checkout returns it to the pool, while removing it from this guard
        // makes any accidental post-rollback SQL fail closed instead of
        // silently running in autocommit mode.
        drop(self.connection.take());
        Ok(())
    }
}

impl Deref for CancellationSafeTransaction {
    type Target = MySqlConnection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_deref()
            .expect("cancellation-safe transaction connection already released")
    }
}

impl DerefMut for CancellationSafeTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.mark_io_in_flight();
        self.connection_mut()
    }
}

impl Drop for CancellationSafeTransaction {
    fn drop(&mut self) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        if self.discard_on_drop {
            connection.close_on_drop();
        } else if self.transaction_open {
            MySqlTransactionManager::start_rollback(connection);
        }
    }
}
