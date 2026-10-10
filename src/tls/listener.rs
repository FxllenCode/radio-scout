//! The HTTPS listener (#76): TCP accepted, TLS handshaken, handed to
//! `axum::serve` as if it were any other listener.
//!
//! # Handshakes happen beside the accept loop, not in it
//!
//! `axum::serve` asks its listener for one connection at a time, so a listener
//! that finished each handshake inside `accept` would let one slow client — a
//! phone on a bad cell, or anyone opening a socket and saying nothing — stall
//! every connection behind it. So TCP is accepted on a task of its own, each
//! handshake runs on its own task under a deadline, and only a finished one is
//! handed over. In-flight handshakes are **bounded** ([`MAX_HANDSHAKES`]),
//! because a Pi answering a flood of half-open connections should run out of
//! patience before it runs out of memory.
//!
//! Everything about a handshake that fails is DEBUG: the internet is mostly
//! scanners probing port 443, and none of it is anything an Operator acts on
//! (ADR-0011 rule 7).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;
use tracing::debug;

/// How long a client has to finish its handshake.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// How many handshakes may be under way at once.
const MAX_HANDSHAKES: usize = 64;

/// TLS connections, ready to serve.
pub struct TlsListener {
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    /// A sender of the listener's own, so the channel cannot close while the
    /// listener exists — `accept` never has a "no more connections, ever"
    /// answer to give `axum::serve`, which has no way to take one.
    _open: mpsc::Sender<(TlsStream<TcpStream>, SocketAddr)>,
    local: SocketAddr,
    /// The accept loop, and every handshake it started — aborted with the
    /// listener, which `axum::serve` drops when it is told to stop.
    accepting: tokio::task::AbortHandle,
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

impl TlsListener {
    /// Accept on `tcp`, completing each handshake with `config`.
    pub fn new(tcp: TcpListener, config: Arc<rustls::ServerConfig>) -> std::io::Result<Self> {
        let local = tcp.local_addr()?;
        let (handed_over, ready) = mpsc::channel(MAX_HANDSHAKES);
        let acceptor = TlsAcceptor::from(config);
        let accepting = tokio::spawn(accept(tcp, acceptor, handed_over.clone())).abort_handle();
        Ok(TlsListener {
            ready,
            _open: handed_over,
            local,
            accepting,
        })
    }
}

/// The accept loop: TCP in, a handshake task out per connection.
async fn accept(
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    handed_over: mpsc::Sender<(TlsStream<TcpStream>, SocketAddr)>,
) {
    let room = Arc::new(Semaphore::new(MAX_HANDSHAKES));
    // Owned here, so aborting this task drops — and so aborts — every
    // handshake it started.
    let mut handshakes = JoinSet::new();
    let mut tcp = tcp;
    loop {
        // axum's own accept for a TCP listener, which already does the right
        // thing with an error — sleeps out a full file-descriptor table rather
        // than spinning on it — so this loop has no error of its own to handle.
        let (stream, peer) = axum::serve::Listener::accept(&mut tcp).await;
        let permit = room
            .clone()
            .acquire_owned()
            .await
            .expect("nothing closes the handshake semaphore");
        let (acceptor, handed_over) = (acceptor.clone(), handed_over.clone());
        handshakes.spawn(async move {
            let _permit = permit;
            match tokio::time::timeout(HANDSHAKE_DEADLINE, acceptor.accept(stream)).await {
                Ok(Ok(tls)) => {
                    // A closed channel is a server that has stopped serving.
                    let _ = handed_over.send((tls, peer)).await;
                }
                Ok(Err(error)) => debug!(%error, "a TLS handshake failed"),
                Err(_) => debug!("a TLS handshake took too long"),
            }
        });
        // Reap what has finished, so a long-running Instance's set holds only
        // what is still under way.
        while handshakes.try_join_next().is_some() {}
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        self.ready
            .recv()
            .await
            .expect("the listener holds a sender, so the channel is open")
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::serve::Listener;
    use tokio::io::AsyncReadExt;

    /// **A client that opens a socket and says nothing is let go** — on a
    /// paused clock, so the ten seconds it is given cost the suite nothing.
    /// Without a deadline it would hold one of the handshake slots for good,
    /// and [`MAX_HANDSHAKES`] of them would stop the Instance answering HTTPS.
    #[tokio::test(start_paused = true)]
    async fn a_silent_client_is_let_go_at_the_deadline() {
        let tcp = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let listener =
            TlsListener::new(tcp, crate::tls::Tls::default().server_config()).expect("listen");
        let addr = listener.local_addr().expect("its address");

        let mut silent = TcpStream::connect(addr).await.expect("connect");
        let mut byte = [0u8; 1];
        let read = silent.read(&mut byte).await;

        assert!(
            matches!(read, Ok(0)) || read.is_err(),
            "the server should have hung up: {read:?}"
        );
        assert_eq!(addr.ip(), std::net::IpAddr::from([127, 0, 0, 1]));
    }
}
