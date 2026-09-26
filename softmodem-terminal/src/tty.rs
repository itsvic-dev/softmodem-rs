// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! A serial device the host already has, such as a UART or a USB gadget.

use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use rustix::fs::{Mode, OFlags};
use rustix::termios::{ControlModes, OptionalActions, tcgetattr, tcsetattr};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Sleep, sleep};
use tracing::{debug, info};

use crate::port::SerialPort;

const REOPEN_AFTER: Duration = Duration::from_secs(1);

/// A tty device that stands in for the modem's serial port, in raw mode at
/// a fixed speed.
///
/// A hang-up of the device, such as DCD dropping or a USB cable coming out,
/// reads as the end of input once, as DTR dropping would be. The port then
/// reopens the device, and reads wait until it opens. While it is closed,
/// writes are lost.
///
/// The device is the DTE end, so carrier is signalled on DTR, which a
/// null-modem cable takes to the computer's DCD and DSR. A device with no
/// control lines, such as Linux's ACM gadget, does not signal it. Ring is
/// never signalled.
#[derive(Debug)]
pub struct TtyPort {
    path: PathBuf,
    speed: u32,
    device: Option<Device>,
    reopen: Option<Pin<Box<Sleep>>>,
    carrier: bool,
}

#[derive(Debug)]
struct Device {
    fd: AsyncFd<OwnedFd>,
    has_lines: bool,
}

impl TtyPort {
    /// Opens the device at `path`, at `speed` bits per second.
    ///
    /// # Errors
    ///
    /// Fails if the device cannot be opened, or is not a tty that takes the
    /// speed.
    pub fn open(path: &Path, speed: u32) -> io::Result<Self> {
        let device = open_raw(path, speed)?;
        Ok(Self {
            path: path.to_owned(),
            speed,
            device: Some(device),
            reopen: None,
            carrier: false,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    // A device that has hung up refuses the change, and the next read finds it gone.
    fn signal_carrier(&self) {
        if let Some(device) = &self.device
            && device.has_lines
            && let Err(error) = set_dtr(device.fd.as_fd(), self.carrier)
        {
            debug!(%error, "could not set DTR");
        }
    }

    fn lose_device(&mut self) {
        self.device = None;
        self.reopen = Some(Box::pin(sleep(REOPEN_AFTER)));
    }

    fn poll_device(&mut self, cx: &mut Context<'_>) -> Poll<&Device> {
        while self.device.is_none() {
            if let Some(reopen) = &mut self.reopen {
                ready!(reopen.as_mut().poll(cx));
            }
            match open_raw(&self.path, self.speed) {
                Ok(device) => {
                    info!(path = %self.path.display(), "serial device open");
                    self.device = Some(device);
                    self.reopen = None;
                    self.signal_carrier();
                }
                Err(error) => {
                    debug!(%error, path = %self.path.display(), "serial device not open");
                    self.lose_device();
                }
            }
        }
        Poll::Ready(self.device.as_ref().expect("the device is open"))
    }
}

fn open_raw(path: &Path, speed: u32) -> io::Result<Device> {
    let fd = rustix::fs::open(
        path,
        OFlags::RDWR | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let mut termios = tcgetattr(&fd)?;
    termios.make_raw();
    termios.control_modes |= ControlModes::CREAD | ControlModes::HUPCL;
    // Without CLOCAL, the kernel hangs the device up when the computer drops DTR.
    termios.control_modes -= ControlModes::CLOCAL | ControlModes::CRTSCTS;
    termios.set_speed(speed)?;
    tcsetattr(&fd, OptionalActions::Now, &termios)?;
    let has_lines = set_dtr(fd.as_fd(), false).is_ok();
    Ok(Device {
        fd: AsyncFd::new(fd)?,
        has_lines,
    })
}

fn set_dtr(fd: BorrowedFd<'_>, on: bool) -> io::Result<()> {
    let request = if on { libc::TIOCMBIS } else { libc::TIOCMBIC };
    let bits: libc::c_int = libc::TIOCM_DTR;
    // SAFETY: TIOCMBIS and TIOCMBIC read one c_int through the pointer.
    if unsafe { libc::ioctl(fd.as_raw_fd(), request, &raw const bits) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl SerialPort for TtyPort {
    fn set_carrier(&mut self, on: bool) -> io::Result<()> {
        self.carrier = on;
        self.signal_carrier();
        Ok(())
    }

    fn takes_another(&self) -> bool {
        true
    }
}

impl AsyncRead for TtyPort {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let read = loop {
            let device = ready!(this.poll_device(cx));
            let mut guard = ready!(device.fd.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            if let Ok(read) = guard.try_io(|fd| Ok(rustix::io::read(fd.get_ref(), &mut *unfilled)?))
            {
                break read;
            }
        };
        match read {
            Ok(0) => info!("serial device hung up"),
            Ok(n) => {
                buf.advance(n);
                return Poll::Ready(Ok(()));
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                info!(%error, "serial device hung up");
            }
            Err(error) => return Poll::Ready(Err(error)),
        }
        this.lose_device();
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for TtyPort {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let Some(device) = &self.get_mut().device else {
            return Poll::Ready(Ok(buf.len()));
        };
        loop {
            let mut guard = ready!(device.fd.poll_write_ready(cx))?;
            match guard.try_io(|fd| Ok(rustix::io::write(fd.get_ref(), buf)?)) {
                Ok(Ok(n)) => return Poll::Ready(Ok(n)),
                // A failed write leaves the device for the next read to find hung up.
                Ok(Err(error)) => {
                    debug!(%error, "lost a write to the computer");
                    return Poll::Ready(Ok(buf.len()));
                }
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
