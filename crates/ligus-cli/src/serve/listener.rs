//! The API's accept loop: hyper's HTTP/1 server with the timeouts
//! `axum::serve` leaves off (serve hardening).
//!
//! - **Header read**: a client has [`super::HEADER_READ_TIMEOUT`] to send a
//!   request's headers. The same clock runs while a keep-alive connection
//!   waits for its next request, so an idle connection is closed too.
//! - **Idle**: a connection that moves no byte either way for
//!   [`super::IDLE_TIMEOUT`] is closed (a stalled upload, a client that
//!   stopped reading). The event stream's 15 s heartbeat keeps a live stream
//!   moving.
//!
//! On shutdown each connection is asked to finish (graceful) and given a
//! few seconds; the loop itself returns at once.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::time::{Instant, Sleep};

/// How long a connection gets to finish once the box stops.
const DRAIN: Duration = Duration::from_secs(5);

/// Serve `app` on `listener` until `stop` turns true.
pub async fn serve(listener: TcpListener, app: Router, header_read: Duration, idle: Duration, mut stop: watch::Receiver<bool>) {
    let (close, closing) = watch::channel(false);
    loop {
        let stream = tokio::select! {
            r = listener.accept() => match r {
                Ok((s, _)) => s,
                Err(e) => {
                    // Out of file descriptors and the like: don't spin.
                    eprintln!("serve: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = stopped(&mut stop) => break,
        };
        let _ = stream.set_nodelay(true);
        let io = TokioIo::new(IdleIo::new(stream, idle));
        let svc = TowerToHyperService::new(app.clone());
        let mut closing = closing.clone();
        tokio::spawn(async move {
            let mut b = hyper::server::conn::http1::Builder::new();
            b.timer(TokioTimer::new()).header_read_timeout(header_read).keep_alive(true);
            let conn = b.serve_connection(io, svc);
            let mut conn = std::pin::pin!(conn);
            tokio::select! {
                _ = conn.as_mut() => {}
                _ = stopped(&mut closing) => {
                    conn.as_mut().graceful_shutdown();
                    let _ = tokio::time::timeout(DRAIN, conn).await;
                }
            }
        });
    }
    let _ = close.send(true);
}

/// Until `rx` turns true (or its sender is gone). Holds no watch guard
/// across an await, so the futures that use it stay `Send`.
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

/// A stream that fails with `TimedOut` once no byte has moved either way
/// for `idle`.
pub struct IdleIo<T> {
    inner: T,
    idle: Duration,
    deadline: Pin<Box<Sleep>>,
}

impl<T> IdleIo<T> {
    pub fn new(inner: T, idle: Duration) -> Self {
        Self { inner, idle, deadline: Box::pin(tokio::time::sleep(idle)) }
    }

    fn moved(&mut self) {
        let at = Instant::now() + self.idle;
        self.deadline.as_mut().reset(at);
    }

    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.deadline.as_mut().poll(cx).is_ready()
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "connection idle")
}

impl<T: AsyncRead + Unpin> AsyncRead for IdleIo<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(r) => {
                if buf.filled().len() > before {
                    self.moved();
                }
                Poll::Ready(r)
            }
            Poll::Pending if self.expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for IdleIo<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, data) {
            Poll::Ready(Ok(n)) => {
                if n > 0 {
                    self.moved();
                }
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending if self.expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(mut self: Pin<&mut Self>, cx: &mut Context<'_>, bufs: &[io::IoSlice<'_>]) -> Poll<io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write_vectored(cx, bufs) {
            Poll::Ready(Ok(n)) => {
                if n > 0 {
                    self.moved();
                }
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending if self.expired(cx) => Poll::Ready(Err(timed_out())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

#[cfg(test)]
mod tests {
    use super::IdleIo;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A quiet stream fails after the idle time; traffic keeps it open.
    #[tokio::test]
    async fn idle_io_times_out_only_when_nothing_moves() {
        let (a, mut b) = tokio::io::duplex(64);
        let mut io = IdleIo::new(a, Duration::from_millis(300));
        let mut buf = [0u8; 4];
        for _ in 0..4 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            b.write_all(b"x").await.unwrap();
            assert_eq!(io.read(&mut buf).await.unwrap(), 1, "traffic resets the clock");
        }
        let err = tokio::time::timeout(Duration::from_secs(5), io.read(&mut buf)).await.expect("ends by itself").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    }
}
