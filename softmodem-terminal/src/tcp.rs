//! A serial port served over TCP, one computer at a time.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tracing::{debug, info};

use crate::port::SerialPort;

/// A listening socket that stands in for the modem's serial port, as QEMU's
/// and UTM's TCP client serial backends expect.
///
/// It serves one connection at a time and accepts the next when that one
/// closes. Its end reads as the end of input once, as DTR dropping would be.
/// While no computer is connected, reads wait for one and writes are lost.
///
/// TCP has no control lines, so carrier and ring are not signalled.
#[derive(Debug)]
pub struct TcpPort {
    listener: TcpListener,
    computer: Option<TcpStream>,
}

impl TcpPort {
    /// Listens on `address`.
    ///
    /// # Errors
    ///
    /// Fails if the address cannot be bound.
    pub async fn bind(address: SocketAddr) -> io::Result<Self> {
        Ok(Self {
            listener: TcpListener::bind(address).await?,
            computer: None,
        })
    }

    /// The address it listens on.
    ///
    /// # Errors
    ///
    /// Fails if the socket cannot report it.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

impl SerialPort for TcpPort {
    fn set_carrier(&mut self, _: bool) -> io::Result<()> {
        Ok(())
    }

    fn takes_another(&self) -> bool {
        true
    }
}

impl AsyncRead for TcpPort {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            let Some(computer) = &mut this.computer else {
                let (stream, peer) = ready!(this.listener.poll_accept(cx))?;
                stream.set_nodelay(true)?;
                info!(%peer, "computer connected");
                this.computer = Some(stream);
                continue;
            };
            let before = buf.filled().len();
            match ready!(Pin::new(computer).poll_read(cx, buf)) {
                Ok(()) if buf.filled().len() > before => return Poll::Ready(Ok(())),
                Ok(()) => info!("computer disconnected"),
                Err(error) => info!(%error, "computer disconnected"),
            }
            this.computer = None;
            return Poll::Ready(Ok(()));
        }
    }
}

impl AsyncWrite for TcpPort {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(computer) = &mut self.get_mut().computer else {
            return Poll::Ready(Ok(buf.len()));
        };
        // A failed write leaves the stream for the next read to find closed.
        match ready!(Pin::new(computer).poll_write(cx, buf)) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(error) => {
                debug!(%error, "lost a write to the computer");
                Poll::Ready(Ok(buf.len()))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let Some(computer) = &mut self.get_mut().computer else {
            return Poll::Ready(Ok(()));
        };
        ready!(Pin::new(computer).poll_flush(cx)).ok();
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
