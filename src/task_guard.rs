use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::task::{JoinError, JoinHandle};

/// Own the task lifetime: dropping a cancelled transfer must stop its workers too.
#[allow(dead_code)]
pub(crate) struct AbortOnDrop<T>(JoinHandle<T>);

#[allow(dead_code)]
impl<T> AbortOnDrop<T> {
    pub fn new(handle: JoinHandle<T>) -> Self { Self(handle) }
    pub fn abort(&self) { self.0.abort(); }
}

impl<T> Future for AbortOnDrop<T> {
    type Output = Result<T, JoinError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) { self.0.abort(); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn drop_aborts_inner_task() {
        let reached = Arc::new(AtomicBool::new(false));
        let flag = reached.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            flag.store(true, Ordering::SeqCst);
        });
        let guard = AbortOnDrop::new(handle);
        drop(guard);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert!(!reached.load(Ordering::SeqCst));
    }
}
