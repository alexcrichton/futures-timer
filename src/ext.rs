//! Extension traits for the standard `Stream` and `Future` traits.

use futures::{Stream, TryFuture, TryStream};
use pin_utils::unsafe_pinned;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::Delay;

/// An extension trait for futures which provides convenient accessors for
/// timing out execution and such.
pub trait TryFutureExt: TryFuture + Sized {
    /// Creates a new future which will take at most `dur` time to resolve from
    /// the point at which this method is called.
    ///
    /// This combinator creates a new future which wraps the receiving future
    /// in a timeout. The future returned will resolve in at most `dur` time
    /// specified (relative to when this function is called).
    ///
    /// If the future completes before `dur` elapses then the future will
    /// resolve with that item. Otherwise the future will resolve to an error
    /// once `dur` has elapsed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use futures::prelude::*;
    /// use futures_timer::{TryFutureExt, TimeoutError};
    ///
    /// # fn long_future() -> impl TryFuture<Ok = (), Error = std::io::Error> {
    /// #     futures::future::ok(())
    /// # }
    /// #
    /// #[runtime::main]
    /// async fn main() {
    ///     let future = long_future();
    ///     let timed_out = future.timeout(Duration::from_secs(1));
    ///
    ///     match timed_out.await {
    ///         Ok(item) => println!("got {:?} within enough time!", item),
    ///         Err(TimeoutError::TimedOut) => println!("took too long to produce the item"),
    ///         Err(TimeoutError::InnerError(e)) => println!("something else went wrong!: {}", e),
    ///     }
    /// }
    /// ```
    fn timeout(self, dur: Duration) -> Timeout<Self> {
        Timeout {
            timeout: Delay::new(dur),
            future: self,
        }
    }

    /// Creates a new future which will resolve no later than `at` specified.
    ///
    /// This method is otherwise equivalent to the `timeout` method except that
    /// it tweaks the moment at when the timeout elapsed to being specified with
    /// an absolute value rather than a relative one. For more documentation see
    /// the `timeout` method.
    fn timeout_at(self, at: Instant) -> Timeout<Self> {
        Timeout {
            timeout: Delay::new_at(at),
            future: self,
        }
    }
}

impl<F: TryFuture> TryFutureExt for F {}

/// Future returned by the `FutureExt::timeout` method.
#[derive(Debug)]
pub struct Timeout<F>
where
    F: TryFuture,
{
    future: F,
    timeout: Delay,
}

impl<F> Timeout<F>
where
    F: TryFuture,
{
    unsafe_pinned!(future: F);
    unsafe_pinned!(timeout: Delay);
}

impl<F> Future for Timeout<F>
where
    F: TryFuture,
{
    type Output = Result<F::Ok, TimeoutError<F::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.as_mut().future().try_poll(cx) {
            Poll::Pending => {}
            other => return other.map_err(TimeoutError::InnerError),
        }

        if self.timeout().poll(cx).is_ready() {
            let err = Err(TimeoutError::TimedOut);
            Poll::Ready(err)
        } else {
            Poll::Pending
        }
    }
}

/// Enum returned by a future with a timeout
#[derive(Debug)]
pub enum TimeoutError<E> {
    /// Variant representing a future which timed out before completion
    TimedOut,

    /// Indicates a future which failed to execute successfully (but did not time out)
    InnerError(E),
}

impl<E> TimeoutError<E> {
    /// Consumes the TimeoutError enum and returns the inner error (if any)
    pub fn into_inner(self) -> Option<E> {
        match self {
            TimeoutError::TimedOut => None,
            TimeoutError::InnerError(e) => Some(e),
        }
    }

    /// Consumes the TimeoutError enum and unwraps the inner error
    pub fn unwrap(self) -> E {
        self.into_inner().unwrap()
    }
}

impl<E> fmt::Display for TimeoutError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TimeoutError::TimedOut => write!(f, "future timed out"),
            TimeoutError::InnerError(e) => write!(f, "inner future error: {}", e),
        }
    }
}

impl<E> Error for TimeoutError<E>
where
    E: Error,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            TimeoutError::TimedOut => None,
            TimeoutError::InnerError(e) => e.source(),
        }
    }
}

/// An extension trait for streams which provides convenient accessors for
/// timing out execution and such.
pub trait TryStreamExt: TryStream + Sized {
    /// Creates a new stream which will take at most `dur` time to yield each
    /// item of the stream.
    ///
    /// This combinator creates a new stream which wraps the receiving stream
    /// in a timeout-per-item. The stream returned will resolve in at most
    /// `dur` time for each item yielded from the stream. The first item's timer
    /// starts when this method is called.
    ///
    /// If a stream's item completes before `dur` elapses then the timer will be
    /// reset for the next item. If the timeout elapses, however, then an error
    /// will be yielded on the stream and the timer will be reset.
    fn timeout(self, dur: Duration) -> TimeoutStream<Self> {
        TimeoutStream {
            timeout: Delay::new(dur),
            dur,
            stream: self,
        }
    }
}

impl<S: TryStream> TryStreamExt for S {}

/// Stream returned by the `StreamExt::timeout` method.
#[derive(Debug)]
pub struct TimeoutStream<S>
where
    S: TryStream,
{
    timeout: Delay,
    dur: Duration,
    stream: S,
}

impl<S> TimeoutStream<S>
where
    S: TryStream,
{
    unsafe_pinned!(timeout: Delay);
    unsafe_pinned!(stream: S);
}

impl<S> Stream for TimeoutStream<S>
where
    S: TryStream,
{
    type Item = Result<S::Ok, TimeoutError<S::Error>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let dur = self.dur;

        let r = self.as_mut().stream().try_poll_next(cx);
        match r {
            Poll::Pending => {}
            Poll::Ready(Some(result)) => {
                self.as_mut().timeout().reset(dur);
                return Poll::Ready(Some(result.map_err(TimeoutError::InnerError)));
            }
            Poll::Ready(None) => {
                self.as_mut().timeout().reset(dur);
                return Poll::Ready(None);
            }
        }

        if self.as_mut().timeout().poll(cx).is_ready() {
            self.as_mut().timeout().reset(dur);
            Poll::Ready(Some(Err(TimeoutError::TimedOut)))
        } else {
            Poll::Pending
        }
    }
}
