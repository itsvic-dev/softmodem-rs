//! The serial port the computer reaches the modem through.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A byte stream to the computer, with the control lines it has.
pub trait SerialPort: AsyncRead + AsyncWrite + Unpin {
    /// Raises or drops DCD, the modem's carrier detect line.
    ///
    /// # Errors
    ///
    /// Fails if the port cannot signal the change.
    fn set_carrier(&mut self, on: bool) -> io::Result<()>;
}

/// A port with no control lines, such as stdin and stdout.
#[derive(Debug)]
pub struct Plain<R, W> {
    pub input: R,
    pub output: W,
}

impl<R: Unpin, W: Unpin> Plain<R, W> {
    fn parts(self: Pin<&mut Self>) -> (Pin<&mut R>, Pin<&mut W>) {
        let this = self.get_mut();
        (Pin::new(&mut this.input), Pin::new(&mut this.output))
    }
}

impl<R: AsyncRead + Unpin, W: Unpin> AsyncRead for Plain<R, W> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.parts().0.poll_read(cx, buf)
    }
}

impl<R: Unpin, W: AsyncWrite + Unpin> AsyncWrite for Plain<R, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.parts().1.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.parts().1.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.parts().1.poll_shutdown(cx)
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> SerialPort for Plain<R, W> {
    fn set_carrier(&mut self, _: bool) -> io::Result<()> {
        Ok(())
    }
}
