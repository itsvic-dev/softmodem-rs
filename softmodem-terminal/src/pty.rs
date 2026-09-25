// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

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

use crate::port::SerialPort;

/// The master end of a raw pseudoterminal. Programs open [`Pty::path`], or
/// the link given to [`Pty::open`], as if it were a serial port.
///
/// It keeps a slave descriptor of its own open, so that a program can close
/// and reopen the port without the master seeing a hang-up.
///
/// A pseudoterminal has no DCD line. Dropping carrier hangs the port up
/// instead, which a program reading it sees as a modem hang-up, and moves the
/// link to a fresh pseudoterminal for the next call.
#[derive(Debug)]
pub struct Pty {
    master: AsyncFd<OwnedFd>,
    slave: OwnedFd,
    path: PathBuf,
    link: Option<PathBuf>,
    carrier: bool,
}

impl Pty {
    /// Opens a pseudoterminal in raw mode, 8-bit clean with no echo, and
    /// points `link` at it if given.
    ///
    /// # Errors
    ///
    /// Fails if no pseudoterminal is available or `link` cannot be created.
    pub fn open(link: Option<&Path>) -> io::Result<Self> {
        let (master, slave, path) = open_raw()?;
        if let Some(link) = link {
            point(link, &path)?;
        }
        Ok(Self {
            master,
            slave,
            path,
            link: link.map(Path::to_owned),
            carrier: false,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Hangs up every program that has the port open, and moves the link to
    /// a fresh pseudoterminal first, so that one reopening it after the
    /// hang-up finds the new one.
    ///
    /// # Errors
    ///
    /// Fails if no pseudoterminal is available or the link cannot be moved.
    pub fn hang_up(&mut self) -> io::Result<()> {
        let (master, slave, path) = open_raw()?;
        if let Some(link) = &self.link {
            point(link, &path)?;
        }
        self.master = master;
        self.slave = slave;
        self.path = path;
        Ok(())
    }
}

fn open_raw() -> io::Result<(AsyncFd<OwnedFd>, OwnedFd, PathBuf)> {
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
    Ok((AsyncFd::new(master)?, slave, path))
}

fn point(link: &Path, target: &Path) -> io::Result<()> {
    let mut staging = link.as_os_str().to_owned();
    staging.push(".new");
    let staging = PathBuf::from(staging);
    match std::fs::remove_file(&staging) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::os::unix::fs::symlink(target, &staging)?;
    std::fs::rename(&staging, link)
}

impl SerialPort for Pty {
    fn set_carrier(&mut self, on: bool) -> io::Result<()> {
        let dropped = self.carrier && !on;
        self.carrier = on;
        if dropped {
            self.hang_up()?;
        }
        Ok(())
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

impl AsyncRead for Pty {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut &*self.get_mut()).poll_read(cx, buf)
    }
}

impl AsyncWrite for Pty {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut &*self.get_mut()).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
