//! Bounded synchronous PostgreSQL connection pooling.
//!
//! A pool can be shared by multiple stores. It never retries a query or other
//! SQL operation; only a new connection is opened when capacity is available.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use postgres::{Client, Config};
use thiserror::Error;

/// Pool failures deliberately omit connector and driver details, which may
/// contain credentials or a database address.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum PoolError {
    /// All available slots remained busy until the acquisition deadline.
    #[error("PostgreSQL connection pool acquisition timed out")]
    Timeout,
    /// The pool has been shut down and accepts no new connections.
    #[error("PostgreSQL connection pool is closed")]
    Closed,
    /// Creating a new connection failed.
    #[error("PostgreSQL connection could not be established")]
    Connect,
}

struct State<T> {
    idle: Vec<T>,
    total: usize,
    closed: bool,
}

struct Inner<T> {
    state: Mutex<State<T>>,
    available: Condvar,
    maximum: usize,
    acquisition_timeout: Duration,
    connect: Box<dyn Fn() -> Result<T, PoolError> + Send + Sync>,
}

/// A bounded pool of synchronous connections.
pub struct ConnectionPool<T> {
    inner: Arc<Inner<T>>,
}

impl<T> std::fmt::Debug for ConnectionPool<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionPool")
            .field("maximum", &self.inner.maximum)
            .field("acquisition_timeout", &self.inner.acquisition_timeout)
            .field("connection_count", &self.connection_count())
            .finish_non_exhaustive()
    }
}

impl<T> Clone for ConnectionPool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> ConnectionPool<T> {
    /// Create an empty pool with a bounded size and acquisition deadline.
    /// The connector is called only when a slot is reserved, outside the pool
    /// lock, and its errors are sanitized to [`PoolError::Connect`].
    pub fn new<F, E>(maximum: usize, acquisition_timeout: Duration, connect: F) -> Self
    where
        F: Fn() -> Result<T, E> + Send + Sync + 'static,
    {
        assert!(maximum > 0, "pool capacity must be positive");
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    idle: Vec::new(),
                    total: 0,
                    closed: false,
                }),
                available: Condvar::new(),
                maximum,
                acquisition_timeout,
                connect: Box::new(move || connect().map_err(|_| PoolError::Connect)),
            }),
        }
    }

    /// Acquire a connection, waiting at most the configured timeout for a
    /// released slot. New SQL operations are never retried by the pool.
    pub fn acquire(&self) -> Result<PooledConnection<T>, PoolError> {
        let deadline = Instant::now() + self.inner.acquisition_timeout;
        loop {
            let mut state = self.inner.state.lock().map_err(|_| PoolError::Closed)?;
            if state.closed {
                return Err(PoolError::Closed);
            }
            if let Some(connection) = state.idle.pop() {
                return Ok(PooledConnection {
                    connection: Some(connection),
                    inner: self.inner.clone(),
                });
            }
            if state.total < self.inner.maximum {
                state.total += 1;
                drop(state);
                match (self.inner.connect)() {
                    Ok(connection) => {
                        // Shutdown may have started while the connector ran.
                        let closed = if let Ok(mut state) = self.inner.state.lock() {
                            if state.closed {
                                state.total = state.total.saturating_sub(1);
                                true
                            } else {
                                false
                            }
                        } else {
                            true
                        };
                        if closed {
                            self.inner.available.notify_one();
                            return Err(PoolError::Closed);
                        }
                        return Ok(PooledConnection {
                            connection: Some(connection),
                            inner: self.inner.clone(),
                        });
                    }
                    Err(error) => {
                        if let Ok(mut state) = self.inner.state.lock() {
                            state.total -= 1;
                        }
                        self.inner.available.notify_one();
                        return Err(error);
                    }
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(PoolError::Timeout);
            }
            let (next, result) = self
                .inner
                .available
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .map_err(|_| PoolError::Closed)?;
            if result.timed_out() && next.idle.is_empty() {
                return Err(if next.closed {
                    PoolError::Closed
                } else {
                    PoolError::Timeout
                });
            }
        }
    }

    /// Stop accepting acquisitions and drop idle connections. Leased
    /// connections are dropped rather than returned when their guards finish.
    pub fn close(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.closed = true;
            state.total = state.total.saturating_sub(state.idle.len());
            state.idle.clear();
        }
        self.inner.available.notify_all();
    }

    /// Number of currently live or being-created connections.
    pub fn connection_count(&self) -> usize {
        self.inner.state.lock().map_or(0, |state| state.total)
    }
}

/// RAII connection lease. Returning it releases the slot to waiting callers.
pub struct PooledConnection<T> {
    connection: Option<T>,
    inner: Arc<Inner<T>>,
}

impl<T> std::fmt::Debug for PooledConnection<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledConnection").finish_non_exhaustive()
    }
}

impl<T> Deref for PooledConnection<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.connection.as_ref().expect("connection lease is live")
    }
}

impl<T> DerefMut for PooledConnection<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.connection.as_mut().expect("connection lease is live")
    }
}

impl<T> Drop for PooledConnection<T> {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if let Ok(mut state) = self.inner.state.lock() {
            if state.closed {
                state.total = state.total.saturating_sub(1);
            } else {
                state.idle.push(connection);
            }
        }
        self.inner.available.notify_one();
    }
}

/// A PostgreSQL pool that uses the shared native-TLS and loopback opt-in policy.
pub type PostgresPool = ConnectionPool<Client>;

/// Construct a shared pool using the verified TLS policy in this module.
pub fn pool(config: Config, maximum: usize, acquisition_timeout: Duration) -> PostgresPool {
    ConnectionPool::new(maximum, acquisition_timeout, move || {
        super::postgres_connection::connect(&config)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;

    #[test]
    fn capacity_is_bounded_and_acquisition_has_a_short_deadline() {
        let pool = ConnectionPool::new(1, Duration::from_millis(25), || Ok::<_, ()>(7));
        let held = pool.acquire().unwrap();
        assert_eq!(pool.connection_count(), 1);
        let start = Instant::now();
        let error = pool.acquire().unwrap_err();
        assert_eq!(error, PoolError::Timeout);
        assert_eq!(
            error.to_string(),
            "PostgreSQL connection pool acquisition timed out"
        );
        assert!(!error.to_string().contains("password"));
        assert!(start.elapsed() >= Duration::from_millis(20));
        drop(held);
        assert_eq!(*pool.acquire().unwrap(), 7);
    }

    #[test]
    fn close_rejects_waiters_and_drops_a_returned_lease() {
        let pool = ConnectionPool::new(1, Duration::from_secs(1), || Ok::<_, ()>(1));
        let lease = pool.acquire().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let waiter_pool = pool.clone();
        let waiter_barrier = barrier.clone();
        let waiter = thread::spawn(move || {
            waiter_barrier.wait();
            waiter_pool.acquire().unwrap_err()
        });
        barrier.wait();
        pool.close();
        assert_eq!(waiter.join().unwrap(), PoolError::Closed);
        drop(lease);
        assert_eq!(pool.connection_count(), 0);
        assert_eq!(pool.acquire().unwrap_err(), PoolError::Closed);
    }

    #[test]
    fn close_drops_idle_connections_immediately() {
        let pool = ConnectionPool::new(1, Duration::from_secs(1), || Ok::<_, ()>(1));
        drop(pool.acquire().unwrap());
        assert_eq!(pool.connection_count(), 1);
        pool.close();
        assert_eq!(pool.connection_count(), 0);
    }

    #[test]
    fn connector_failures_are_sanitized() {
        let pool = ConnectionPool::<usize>::new(1, Duration::from_millis(5), || {
            Err::<usize, _>("postgres://user:secret@database")
        });
        let error = pool.acquire().unwrap_err();
        assert_eq!(error, PoolError::Connect);
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("database"));
        assert_eq!(pool.connection_count(), 0);
    }
}
