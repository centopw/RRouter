//! Streaming fallback: identical retry policy to [`crate::Dispatcher::run`], but
//! a candidate is only "failed" if it errors **before** the first body byte.
//!
//! Once a chunk reaches the client the stream is committed. A retry after
//! partial output would duplicate content, so mid-stream failures are converted
//! into a terminal `Err` on the returned stream — the gateway turns that into a
//! single error event and closes the response.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::stream::BoxStream;
use futures_util::{Stream, StreamExt};
use rrouter_core::{Error, Result};

use crate::breaker::Breaker;
use crate::candidate::Candidate;
use crate::dispatch::{no_route, retry_after, status_of};
use crate::Dispatcher;

/// Upstream body chunks for one streamed response.
pub type ByteStream = BoxStream<'static, Result<Bytes>>;

impl Dispatcher {
    /// Streaming fallback run. `open` performs exactly one upstream request and
    /// returns as soon as response **headers** are available (body not read).
    pub async fn run_streaming<F, Fut>(
        &self,
        plan: &crate::candidate::Plan,
        open: F,
    ) -> Result<(Candidate, ByteStream)>
    where
        F: Fn(Candidate) -> Fut,
        Fut: Future<Output = Result<ByteStream>>,
    {
        let mut order = self.attempt_order(plan).await;
        if order.is_empty() {
            return Err(no_route(plan, "every candidate is blocked by its circuit breaker"));
        }

        let mut last: Option<Error> = None;
        let mut sends = 0usize;
        while sends < self.max_attempts() {
            let Some((candidate, mut claim)) = self.next_claim(plan, &mut order).await else {
                break;
            };
            sends += 1;
            let opened = open(candidate.clone()).await;
            claim.disarm();
            match opened {
                Ok(stream) => {
                    let committed =
                        Committed::new(stream, self.breaker_arc(), candidate.connection);
                    return Ok((candidate, Box::pin(committed)));
                }
                Err(err) if !err.is_retryable_upstream() => {
                    // The account answered sanely; the request was bad. Release the probe.
                    self.breaker.record_success(candidate.connection);
                    return Err(err);
                }
                Err(err) => {
                    self.breaker
                        .record_failure(candidate.connection, status_of(&err), retry_after(&err));
                    last = Some(err);
                }
            }
        }
        Err(last.unwrap_or_else(|| no_route(plan, "attempt cap reached")))
    }
}

/// Wraps the winning stream so breaker accounting reflects what the body did,
/// and mid-stream failures become a single terminal error.
///
/// The gateway's own encoder stops polling the upstream once it has decoded the
/// terminal event, so this stream may be dropped without ever returning
/// `Ready(None)`. Success is therefore settled on `Drop` unless the stream
/// already reported a failure — otherwise a streamed success would leave a
/// claimed half-open probe slot taken forever.
struct Committed {
    inner: ByteStream,
    breaker: std::sync::Arc<Breaker>,
    conn: uuid::Uuid,
    /// `true` once success *or* failure has been reported to the breaker.
    settled: bool,
}

impl Committed {
    fn new(inner: ByteStream, breaker: std::sync::Arc<Breaker>, conn: uuid::Uuid) -> Self {
        Self { inner, breaker, conn, settled: false }
    }

    fn settle_success(&mut self) {
        if !self.settled {
            self.settled = true;
            self.breaker.record_success(self.conn);
        }
    }
}

impl Drop for Committed {
    fn drop(&mut self) {
        // Reaching drop without a reported failure means the body was consumed
        // (or the client went away after receiving output). Either way the
        // credential worked, so close the breaker rather than leak the probe.
        self.settle_success();
    }
}

impl Stream for Committed {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.settled {
            return Poll::Ready(None);
        }
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Err(err))) => {
                this.settled = true;
                // Mid-stream: the commit point has passed, so record the failure
                // and surface it. The gateway emits one error event and stops.
                this.breaker.record_failure(this.conn, status_of(&err), retry_after(&err));
                Poll::Ready(Some(Err(err)))
            }
            Poll::Ready(None) => {
                this.settle_success();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Convenience constructor for tests and callers that already hold a stream.
pub fn byte_stream<S>(stream: S) -> ByteStream
where
    S: Stream<Item = Result<Bytes>> + Send + 'static,
{
    Box::pin(stream)
}

/// Drain a byte stream to completion, stopping at the first error.
pub async fn collect_stream(mut stream: ByteStream) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}
