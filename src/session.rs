//! [`BoundedSessionManager`] — a [`SessionManager`] wrapper that enforces a
//! maximum concurrent session count with FIFO eviction and optional rate
//! limiting on session creation.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use futures_core::Stream;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::{
    streamable_http_server::session::{
        local::{LocalSessionManager, LocalSessionManagerError, LocalSessionWorker, SessionConfig},
        ServerSseMessage, SessionId, SessionManager,
    },
    WorkerTransport,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by [`BoundedSessionManager`].
#[derive(Debug, thiserror::Error)]
pub enum BoundedSessionError {
    /// Propagated from the inner [`LocalSessionManager`].
    #[error(transparent)]
    Inner(#[from] LocalSessionManagerError),
    /// Session creation was rejected because the rate limit was exceeded.
    #[error("session creation rate limit exceeded")]
    RateLimited,
}

// ---------------------------------------------------------------------------
// BoundedSessionManager
// ---------------------------------------------------------------------------

/// Wraps [`LocalSessionManager`] and limits the number of concurrent sessions.
///
/// When the limit is reached, the oldest session (by creation order) is closed
/// before the new one is created. This prevents unbounded memory growth when
/// many clients connect without explicitly closing their sessions.
///
/// Optionally, a rate limit can be applied to session creation via
/// [`BoundedSessionManager::with_rate_limit`].
pub struct BoundedSessionManager {
    inner: LocalSessionManager,
    max_sessions: usize,
    /// Tracks session IDs in creation order for FIFO eviction.
    creation_order: tokio::sync::Mutex<VecDeque<SessionId>>,
    /// Ring buffer of recent creation timestamps for rate limiting.
    rate_tracker: tokio::sync::Mutex<VecDeque<Instant>>,
    /// Optional rate limit: (max_creates, window_duration).
    rate_limit: Option<(usize, Duration)>,
}

impl BoundedSessionManager {
    /// Create a new `BoundedSessionManager`.
    ///
    /// * `session_config` — passed through to the inner [`LocalSessionManager`].
    /// * `max_sessions`   — maximum number of concurrent sessions. When this
    ///   limit is reached, the oldest session is evicted before creating a new
    ///   one. Must be at least 1.
    ///
    /// # Panics
    ///
    /// Panics if `max_sessions` is 0.
    pub fn new(session_config: SessionConfig, max_sessions: usize) -> Self {
        assert!(max_sessions >= 1, "max_sessions must be at least 1, got 0");
        Self {
            inner: LocalSessionManager {
                session_config,
                ..Default::default()
            },
            max_sessions,
            creation_order: tokio::sync::Mutex::new(VecDeque::new()),
            rate_tracker: tokio::sync::Mutex::new(VecDeque::new()),
            rate_limit: None,
        }
    }

    /// Configure a rate limit on session creation.
    ///
    /// At most `max_creates` sessions may be created within any rolling
    /// `window` duration. If exceeded, [`BoundedSessionError::RateLimited`] is
    /// returned and no eviction is performed.
    #[must_use]
    pub fn with_rate_limit(mut self, max_creates: usize, window: Duration) -> Self {
        self.rate_limit = Some((max_creates, window));
        self
    }
}

impl SessionManager for BoundedSessionManager {
    type Error = BoundedSessionError;
    type Transport = WorkerTransport<LocalSessionWorker>;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        // ----------------------------------------------------------------
        // Critical section 1: rate-limit check.
        // ----------------------------------------------------------------
        if let Some((max_creates, window)) = self.rate_limit {
            let mut rate = self.rate_tracker.lock().await;
            let now = Instant::now();
            // Prune entries that have fallen outside the window.
            while rate
                .front()
                .is_some_and(|t| now.duration_since(*t) > window)
            {
                rate.pop_front();
            }
            if rate.len() >= max_creates {
                return Err(BoundedSessionError::RateLimited);
            }
            rate.push_back(now);
        }

        // ----------------------------------------------------------------
        // Determine eviction candidate (short critical section).
        // ----------------------------------------------------------------
        let evict_candidate = {
            let order = self.creation_order.lock().await;
            // Use the inner sessions map for the authoritative live count so
            // that expired sessions (which are removed from inner but remain
            // in the deque) do not consume a capacity slot.
            let live_count = self.inner.sessions.read().await.len();
            if live_count >= self.max_sessions {
                order.front().cloned()
            } else {
                None
            }
        };

        // ----------------------------------------------------------------
        // Evict oldest (no lock held across this await).
        // ----------------------------------------------------------------
        if let Some(ref oldest) = evict_candidate {
            // Ignore errors: the session may have already expired.
            let _ = self.inner.close_session(oldest).await;
        }

        // ----------------------------------------------------------------
        // Create new session (no lock held across this await).
        // ----------------------------------------------------------------
        let (id, transport) = self.inner.create_session().await?;

        // ----------------------------------------------------------------
        // Critical section 2: update the creation-order deque.
        // ----------------------------------------------------------------
        {
            let mut order = self.creation_order.lock().await;
            // Remove the evicted entry if it's still present.
            if let Some(ref oldest) = evict_candidate {
                order.retain(|s| s != oldest);
            }
            // Prune any deque entries for sessions that are no longer live
            // (handles the drift caused by keep_alive expiry: finding #4).
            let live_ids: std::collections::HashSet<_> = {
                // Snapshot the live session IDs without holding two locks
                // simultaneously (creation_order lock is already held here;
                // sessions is a RwLock so a read lock is fine).
                self.inner.sessions.read().await.keys().cloned().collect()
            };
            order.retain(|s| live_ids.contains(s));
            order.push_back(id.clone());
        }

        Ok((id, transport))
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.inner.close_session(id).await?;
        let mut order = self.creation_order.lock().await;
        order.retain(|s| s != id);
        Ok(())
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        self.inner
            .initialize_session(id, message)
            .await
            .map_err(Into::into)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        self.inner.has_session(id).await.map_err(Into::into)
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner
            .create_stream(id, message)
            .await
            .map_err(Into::into)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner
            .accept_message(id, message)
            .await
            .map_err(Into::into)
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner
            .create_standalone_stream(id)
            .await
            .map_err(Into::into)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner
            .resume(id, last_event_id)
            .await
            .map_err(Into::into)
    }
}
