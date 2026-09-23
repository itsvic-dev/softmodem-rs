//! A pseudoterminal that stands in for the modem's serial port.

use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use rustix::fs::{Mode, OFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The master end of a raw pseudoterminal. Programs open [`Pty::path`], or
/// the link given to [`Pty::open`], as if it were a serial port.
///
/// It keeps a slave descriptor of its own open, so that a program can close
/// and reopen the port without the master seeing a hang-up.
#[derive(Debug)]
pub struct Pty {
    master: AsyncFd<OwnedFd>,
    _slave: OwnedFd,
    path: PathBuf,
    link: Option<PathBuf>,
}

impl Pty {
    /// Opens a pseudoterminal in raw mode, 8-bit clean with no echo, and
    /// points `link` at it if given.
    ///
    /// # Errors
    ///
    /// Fails if no pseudoterminal is available or `link` cannot be created.
    pub fn open(link: Option<&Path>) -> io::Result<Self> {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        rustix::pty::grantpt(&master)?;
        rustix::pty::unlockpt(&master)?;
        let path = PathBuf::from(OsStr::from_bytes(
            rustix::pty::ptsname(&master, Vec::new())?.as_bytes(),
        ));
        let slave = rustix::fs::open(
            &path,
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let mut termios = tcgetattr(&slave)?;
        termios.make_raw();
        tcsetattr(&slave, OptionalActions::Now, &termios)?;
        rustix::io::ioctl_fionbio(&master, true)?;

        if let Some(link) = link {
            match std::fs::remove_file(link) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            std::os::unix::fs::symlink(&path, link)?;
        }

        Ok(Self {
            master: AsyncFd::new(master)?,
            _slave: slave,
            path,
            link: link.map(Path::to_owned),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            let _ = std::fs::remove_file(link);
        }
    }
}

impl AsyncRead for &Pty {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        loop {
            let mut guard = ready!(self.master.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match guard.try_io(|fd| Ok(rustix::io::read(fd.get_ref(), &mut *unfilled)?)) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return Poll::Ready(Err(error)),
                Err(_would_block) => {}
            }
        }
    }
}

impl AsyncWrite for &Pty {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = ready!(self.master.poll_write_ready(cx))?;
            match guard.try_io(|fd| Ok(rustix::io::write(fd.get_ref(), buf)?)) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => {}
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
