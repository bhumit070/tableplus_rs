//! Stopping a running editor batch. Where the server supports it (Postgres
//! `pg_cancel_backend`, MySQL `KILL QUERY`), the running statement is
//! cancelled server-side first, so it doesn't keep working after we stop
//! waiting; then the batch is abandoned.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use tokio::sync::Notify;

/// The error message of a cancelled run.
pub const CANCELLED: &str = "Query cancelled";

type ServerCancel = Box<dyn FnOnce() -> BoxFuture<'static, Result<(), String>> + Send>;

#[derive(Default)]
pub struct Cancel {
    /// Set when cancelling starts.
    flag: AtomicBool,
    /// Set once the server-side cancel has had its chance.
    done: AtomicBool,
    notify: Notify,
    server: Mutex<Option<ServerCancel>>,
    /// Why the server-side cancel didn't go through, if it didn't.
    server_error: Mutex<Option<String>>,
}

impl Cancel {
    /// Registers how to cancel the statement running now, server-side.
    pub fn on_server_cancel(&self, f: ServerCancel) {
        *self.server.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    }

    /// The message for a cancelled run: says when the server may still be
    /// running the statement.
    pub fn message(&self) -> String {
        match self.server_error.lock().unwrap_or_else(|e| e.into_inner()).as_deref() {
            Some(e) => format!("{CANCELLED}. The server may still be running it ({e}); see Processes (⌘⇧P)."),
            None => CANCELLED.to_string(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// Cancels server-side (if registered, at most 5 s), then wakes
    /// `cancelled()`.
    pub async fn cancel(&self) {
        if self.flag.swap(true, Ordering::SeqCst) {
            return;
        }
        let server = self.server.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(f) = server {
            let err = match tokio::time::timeout(Duration::from_secs(5), f()).await {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some("the server didn't answer the cancel request".into()),
            };
            *self.server_error.lock().unwrap_or_else(|e| e.into_inner()) = err;
        }
        self.done.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Resolves once `cancel` has run.
    pub async fn cancelled(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        // Register before checking, so a cancel in between isn't missed.
        notified.as_mut().enable();
        if !self.done.load(Ordering::SeqCst) {
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn cancel_runs_the_server_side_cancel_then_wakes_waiters() {
        let c = Arc::new(Cancel::default());
        let hit = Arc::new(AtomicBool::new(false));
        let h = hit.clone();
        c.on_server_cancel(Box::new(move || {
            Box::pin(async move {
                h.store(true, Ordering::SeqCst);
                Ok(())
            })
        }));
        let waiter = tokio::spawn({
            let c = c.clone();
            async move { c.cancelled().await }
        });
        c.cancel().await;
        tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("woken")
            .unwrap();
        assert!(hit.load(Ordering::SeqCst));
        assert!(c.is_cancelled());
    }
}
