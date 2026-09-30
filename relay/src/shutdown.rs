//! Stopping a relay without cutting everybody off mid-sentence.
//!
//! A redeploy sends `SIGTERM`. Killing the process there and then drops every
//! socket and every chunk upload at once. Neither loses data — clients resume
//! and uploads are idempotent — but every device then finds out by timing out
//! rather than by being told, and they all reconnect in the same instant.
//!
//! So a stop has two halves. The listener stops taking connections and lets the
//! HTTP requests already in flight finish; axum does that. The sync sockets are
//! not requests — once upgraded, axum no longer tracks them — so each session
//! watches the signal here, and on it sends what it has queued, closes its
//! socket cleanly, and goes. [`Shutdown::drained`] is when every one of them
//! has.

use tokio::sync::watch;

/// The relay's stop signal, and a count of who is still listening for it.
pub struct Shutdown {
    signal: watch::Sender<bool>,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self {
            signal: watch::channel(false).0,
        }
    }
}

impl Shutdown {
    /// Tell every session, and the listener, to finish. Idempotent.
    pub fn begin(&self) {
        self.signal.send_replace(true);
    }

    /// Whether a stop has begun. A socket asked for after this is refused,
    /// because it would only be closed again straight away.
    pub fn is_draining(&self) -> bool {
        *self.signal.borrow()
    }

    /// Something that has to finish before the relay has drained. Hold it for
    /// as long as that is true: [`Shutdown::drained`] waits for every one to be
    /// dropped.
    pub(crate) fn watch(&self) -> Watch {
        Watch(self.signal.subscribe())
    }

    /// Resolves once every [`Watch`] has been dropped. Immediately, if none is
    /// held; so only meaningful after [`Shutdown::begin`], before which a new
    /// session could still start.
    pub async fn drained(&self) {
        self.signal.closed().await;
    }
}

/// One session's hold on the relay's stop. Dropping it says the session is
/// done.
pub(crate) struct Watch(watch::Receiver<bool>);

impl Watch {
    /// Resolves when a stop begins, or at once if it already has. Cancel-safe.
    pub(crate) async fn begun(&mut self) {
        // The sender lives in the relay's state, which every holder of a watch
        // keeps alive, so an error here cannot happen; and if it could, a
        // relay with no one left to stop it has stopped.
        let _ = self.0.wait_for(|stopping| *stopping).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn drained_waits_for_every_watch_to_go() {
        let shutdown = Shutdown::default();
        let mut session = shutdown.watch();
        assert!(!shutdown.is_draining());

        shutdown.begin();
        assert!(shutdown.is_draining());
        // Begun, but the session still holds on.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), shutdown.drained())
                .await
                .is_err()
        );

        session.begun().await;
        drop(session);
        tokio::time::timeout(Duration::from_secs(1), shutdown.drained())
            .await
            .expect("drained once the last watch is dropped");
    }

    #[tokio::test]
    async fn a_watch_taken_after_the_stop_sees_it_at_once() {
        let shutdown = Shutdown::default();
        shutdown.begin();
        let mut late = shutdown.watch();
        tokio::time::timeout(Duration::from_secs(1), late.begun())
            .await
            .expect("a late session is told straight away");
    }
}
