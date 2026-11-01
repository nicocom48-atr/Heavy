use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::task::{JoinError, JoinHandle};

/// Own the task lifetime: dropping a cancelled transfer must stop its workers too.
pub(crate) struct AbortOnDrop<T>(JoinHandle<T>);

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
