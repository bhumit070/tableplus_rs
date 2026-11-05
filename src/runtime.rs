//! gpui has its own executor; the drivers need tokio. DB work runs on this
//! runtime and the UI awaits the result through a oneshot channel.

use std::future::Future;
use std::sync::LazyLock;

use tokio::sync::oneshot;

static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .thread_name("tabula-db")
        .build()
        .expect("tokio runtime")
});

/// Runs `fut` on tokio; await the returned future from `cx.spawn`.
pub fn spawn<T: Send + 'static>(
    fut: impl Future<Output = anyhow::Result<T>> + Send + 'static,
) -> impl Future<Output = anyhow::Result<T>> {
    let (tx, rx) = oneshot::channel();
    RT.spawn(async move {
        let _ = tx.send(fut.await);
    });
    async move {
        rx.await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("task cancelled")))
    }
}

/// Runs `fut` on tokio without waiting for it.
pub fn spawn_detached(fut: impl Future<Output = ()> + Send + 'static) {
    RT.spawn(fut);
}
