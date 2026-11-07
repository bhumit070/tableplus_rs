//! gpui has its own executor; the drivers need tokio. DB work runs on this
//! runtime and the UI awaits the result through a oneshot channel.

use std::future::Future;
use std::sync::LazyLock;

use anyhow::{Result, anyhow};
use tokio::sync::oneshot;
use tokio::{runtime::Builder, runtime::Runtime, spawn as tokio_spawn};

static RT: LazyLock<Runtime> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .thread_name("tabula-db")
        .build()
        .expect("tokio runtime")
});

/// Runs `fut` on tokio; await the returned future from `cx.spawn`.
pub fn spawn<T: Send + 'static>(
    fut: impl Future<Output = Result<T>> + Send + 'static,
) -> impl Future<Output = Result<T>> {
    let (tx, rx) = oneshot::channel();
    RT.spawn(async move {
        // A panic in DB code becomes an error the UI can show.
        let out = match tokio_spawn(fut).await {
            Ok(r) => r,
            Err(e) if e.is_panic() => {
                let p = e.into_panic();
                let msg = p
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| p.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown".into());
                Err(anyhow!("Internal error (a bug in Tabula): {msg}"))
            }
            Err(_) => Err(anyhow!("task cancelled")),
        };
        let _ = tx.send(out);
    });
    async move { rx.await.unwrap_or_else(|_| Err(anyhow!("task cancelled"))) }
}

/// Runs `fut` on tokio without waiting for it.
pub fn spawn_detached(fut: impl Future<Output = ()> + Send + 'static) {
    RT.spawn(fut);
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use futures::executor::block_on;

    #[test]
    fn panics_become_errors() {
        let r: Result<()> = block_on(super::spawn(async {
            if true {
                panic!("boom");
            }
            Ok(())
        }));
        assert!(r.unwrap_err().to_string().contains("boom"));
    }
}
