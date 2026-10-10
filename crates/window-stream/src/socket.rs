//! Splits one socket into a read half and a write half that share it.
//!
//! `futures_util`'s `split` buffers one item in its write half and reports
//! ready whenever that buffer is empty, whatever the socket says. A session
//! must encode only when the socket itself can take the next message, so
//! these halves forward readiness unchanged and buffer nothing.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures_util::{Sink, Stream};

use crate::lock;

pub(crate) struct ReadHalf<S>(Arc<Mutex<S>>);
pub(crate) struct WriteHalf<S>(Arc<Mutex<S>>);

pub(crate) fn split<S>(socket: S) -> (ReadHalf<S>, WriteHalf<S>) {
    let shared = Arc::new(Mutex::new(socket));
    (ReadHalf(shared.clone()), WriteHalf(shared))
}

impl<S: Stream + Unpin> Stream for ReadHalf<S> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<S::Item>> {
        Pin::new(&mut *lock(&self.0)).poll_next(cx)
    }
}

impl<S: Sink<M> + Unpin, M> Sink<M> for WriteHalf<S> {
    type Error = S::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        Pin::new(&mut *lock(&self.0)).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: M) -> Result<(), S::Error> {
        Pin::new(&mut *lock(&self.0)).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        Pin::new(&mut *lock(&self.0)).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        Pin::new(&mut *lock(&self.0)).poll_close(cx)
    }
}
