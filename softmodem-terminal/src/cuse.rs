// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! A serial port served as a character device through CUSE, for programs
//! that read the modem lines with `TIOCMGET`, such as QEMU's and 86Box's host
//! serial backends. Opening one needs Linux and access to `/dev/cuse`.
//!
//! The wire format is the FUSE kernel protocol, as in `linux/fuse.h`.

use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::pin::Pin;
use std::task::{Context, Poll};

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::sync::watch;
use tracing::{debug, warn};

use crate::port::SerialPort;

const FUSE_KERNEL_VERSION: u32 = 7;
const FUSE_KERNEL_MINOR: u32 = 31;
const CUSE_UNRESTRICTED_IOCTL: u32 = 1;
const FOPEN_DIRECT_IO: u32 = 1;
const FOPEN_NONSEEKABLE: u32 = 4;
const FUSE_IOCTL_RETRY: u32 = 4;
const FUSE_POLL_SCHEDULE_NOTIFY: u32 = 1;
const FUSE_NOTIFY_POLL: i32 = 1;

const FUSE_OPEN: u32 = 14;
const FUSE_READ: u32 = 15;
const FUSE_WRITE: u32 = 16;
const FUSE_RELEASE: u32 = 18;
const FUSE_FSYNC: u32 = 20;
const FUSE_FLUSH: u32 = 25;
const FUSE_INTERRUPT: u32 = 36;
const FUSE_IOCTL: u32 = 39;
const FUSE_POLL: u32 = 40;
const CUSE_INIT: u32 = 4096;

const IN_HEADER: usize = 40;
const REQUEST_BUFFER: usize = 1 << 17;
const MAX_TRANSFER: u32 = 1 << 16;
const BUFFER_LIMIT: usize = 1 << 16;
const O_NONBLOCK: u32 = 0o4000;

// Linux ioctl numbers and flags, the same on x86 and arm.
const TCGETS: u32 = 0x5401;
const TCSETS: u32 = 0x5402;
const TCSETSW: u32 = 0x5403;
const TCSETSF: u32 = 0x5404;
const TCSBRK: u32 = 0x5409;
const TCXONC: u32 = 0x540A;
const TCFLSH: u32 = 0x540B;
const TIOCEXCL: u32 = 0x540C;
const TIOCNXCL: u32 = 0x540D;
const TIOCOUTQ: u32 = 0x5411;
const TIOCMGET: u32 = 0x5415;
const TIOCMBIS: u32 = 0x5416;
const TIOCMBIC: u32 = 0x5417;
const TIOCMSET: u32 = 0x5418;
const FIONREAD: u32 = 0x541B;
const TIOCSBRK: u32 = 0x5427;
const TIOCCBRK: u32 = 0x5428;
const TCGETS2: u32 = 0x802C_542A;
const TCSETS2: u32 = 0x402C_542B;
const TCSETSW2: u32 = 0x402C_542C;
const TCSETSF2: u32 = 0x402C_542D;
const TIOCM_DTR: u32 = 0x002;
const TIOCM_RTS: u32 = 0x004;
const TIOCM_CTS: u32 = 0x020;
const TIOCM_CAR: u32 = 0x040;
const TIOCM_RNG: u32 = 0x080;
const TIOCM_DSR: u32 = 0x100;
const POLLIN: u32 = 0x001;
const POLLOUT: u32 = 0x004;
const POLLRDNORM: u32 = 0x040;
const POLLWRNORM: u32 = 0x100;
const TERMIOS: usize = 36;
const TERMIOS2: usize = 44;

#[derive(Debug, Clone, Copy, Default)]
struct Lines {
    carrier: bool,
    ring: bool,
}

/// The modem's end of a CUSE serial port.
#[derive(Debug)]
pub struct CusePort {
    stream: DuplexStream,
    lines: watch::Sender<Lines>,
}

impl CusePort {
    /// Creates `/dev/<name>` and serves it until the port is dropped.
    ///
    /// # Errors
    ///
    /// Fails if `/dev/cuse` cannot be opened, which is so on any system but
    /// Linux and without the access it needs.
    pub fn open(name: &str) -> io::Result<Self> {
        let fd = rustix::fs::open(
            "/dev/cuse",
            OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let (modem_side, server_side) = tokio::io::duplex(4096);
        let (lines, watched) = watch::channel(Lines::default());
        let server = Server {
            fd: AsyncFd::new(fd)?,
            name: name.to_owned(),
            termios: default_termios(),
            computer_lines: TIOCM_DTR | TIOCM_RTS,
            lines: Lines::default(),
            to_computer: VecDeque::new(),
            to_modem: Vec::new(),
            waiting_reads: VecDeque::new(),
            poll_handle: None,
        };
        tokio::spawn(async move {
            if let Err(error) = server.run(server_side, watched).await {
                warn!(%error, "CUSE serial port stopped");
            }
        });
        Ok(Self {
            stream: modem_side,
            lines,
        })
    }
}

impl SerialPort for CusePort {
    fn set_carrier(&mut self, on: bool) -> io::Result<()> {
        self.lines.send_modify(|lines| lines.carrier = on);
        Ok(())
    }

    fn set_ring(&mut self, on: bool) -> io::Result<()> {
        self.lines.send_modify(|lines| lines.ring = on);
        Ok(())
    }
}

impl AsyncRead for CusePort {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for CusePort {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

struct Server {
    fd: AsyncFd<OwnedFd>,
    name: String,
    termios: [u8; TERMIOS2],
    computer_lines: u32,
    lines: Lines,
    to_computer: VecDeque<u8>,
    to_modem: Vec<u8>,
    waiting_reads: VecDeque<(u64, usize)>,
    poll_handle: Option<u64>,
}

struct Request<'a> {
    opcode: u32,
    unique: u64,
    body: &'a [u8],
}

struct Ioctl<'a> {
    unique: u64,
    argument: u64,
    input: &'a [u8],
    out_size: usize,
}

impl Server {
    async fn run(
        mut self,
        stream: DuplexStream,
        mut watched: watch::Receiver<Lines>,
    ) -> io::Result<()> {
        let (mut from_modem, mut into_modem) = tokio::io::split(stream);
        let mut buf = vec![0; REQUEST_BUFFER];
        let mut chunk = [0; 512];
        loop {
            let room = self.to_computer.len() < BUFFER_LIMIT;
            tokio::select! {
                n = next_request(&self.fd, &mut buf) => {
                    let n = n?;
                    self.handle(&buf[..n])?;
                }
                read = from_modem.read(&mut chunk), if room => {
                    let n = read?;
                    if n == 0 {
                        return Ok(());
                    }
                    self.to_computer.extend(&chunk[..n]);
                    self.serve_waiting_reads()?;
                    self.wake_poll()?;
                }
                written = into_modem.write(&self.to_modem), if !self.to_modem.is_empty() => {
                    let n = written?;
                    self.to_modem.drain(..n);
                }
                changed = watched.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    self.lines = *watched.borrow();
                }
            }
        }
    }

    fn handle(&mut self, message: &[u8]) -> io::Result<()> {
        let Some(request) = parse_request(message) else {
            return Ok(());
        };
        match request.opcode {
            CUSE_INIT => self.init(&request),
            FUSE_OPEN => {
                let flags = FOPEN_DIRECT_IO | FOPEN_NONSEEKABLE;
                self.reply(
                    request.unique,
                    0,
                    &[&0u64.to_ne_bytes(), &flags.to_ne_bytes(), &[0; 4]],
                )
            }
            FUSE_RELEASE | FUSE_FLUSH | FUSE_FSYNC => self.reply(request.unique, 0, &[]),
            FUSE_READ => self.read(&request),
            FUSE_WRITE => self.write(&request),
            FUSE_IOCTL => self.ioctl(&request),
            FUSE_POLL => self.poll(&request),
            FUSE_INTERRUPT => self.interrupt(&request),
            opcode => {
                debug!(opcode, "unsupported CUSE request");
                self.reply(request.unique, -Errno::NOSYS.raw_os_error(), &[])
            }
        }
    }

    fn init(&self, request: &Request<'_>) -> io::Result<()> {
        let kernel_minor = u32_at(request.body, 4).unwrap_or(0);
        let mut out = Vec::with_capacity(72);
        for field in [
            FUSE_KERNEL_VERSION,
            kernel_minor.min(FUSE_KERNEL_MINOR),
            0,
            CUSE_UNRESTRICTED_IOCTL,
            MAX_TRANSFER,
            MAX_TRANSFER,
            0,
            0,
        ] {
            out.extend(field.to_ne_bytes());
        }
        out.extend([0; 40]);
        let info = format!("DEVNAME={}\0", self.name);
        self.reply(request.unique, 0, &[&out, info.as_bytes()])
    }

    fn read(&mut self, request: &Request<'_>) -> io::Result<()> {
        let size = u32_at(request.body, 16).unwrap_or(0) as usize;
        let flags = u32_at(request.body, 32).unwrap_or(0);
        if !self.to_computer.is_empty() {
            let data = self.take(size);
            return self.reply(request.unique, 0, &[&data]);
        }
        if flags & O_NONBLOCK != 0 {
            return self.reply(request.unique, -Errno::AGAIN.raw_os_error(), &[]);
        }
        self.waiting_reads.push_back((request.unique, size));
        Ok(())
    }

    fn write(&mut self, request: &Request<'_>) -> io::Result<()> {
        let size = u32_at(request.body, 16).unwrap_or(0) as usize;
        let data = request.body.get(40..40 + size).unwrap_or_default();
        if self.to_modem.len() + data.len() > BUFFER_LIMIT {
            return self.reply(request.unique, -Errno::AGAIN.raw_os_error(), &[]);
        }
        self.to_modem.extend_from_slice(data);
        let written = u32::try_from(data.len()).unwrap_or(0);
        self.reply(request.unique, 0, &[&written.to_ne_bytes(), &[0; 4]])
    }

    fn poll(&mut self, request: &Request<'_>) -> io::Result<()> {
        let handle = u64_at(request.body, 8).unwrap_or(0);
        let flags = u32_at(request.body, 16).unwrap_or(0);
        if flags & FUSE_POLL_SCHEDULE_NOTIFY != 0 {
            self.poll_handle = Some(handle);
        }
        let mut events = POLLOUT | POLLWRNORM;
        if !self.to_computer.is_empty() {
            events |= POLLIN | POLLRDNORM;
        }
        self.reply(request.unique, 0, &[&events.to_ne_bytes(), &[0; 4]])
    }

    fn interrupt(&mut self, request: &Request<'_>) -> io::Result<()> {
        let target = u64_at(request.body, 0).unwrap_or(0);
        if let Some(at) = self
            .waiting_reads
            .iter()
            .position(|(unique, _)| *unique == target)
        {
            self.waiting_reads.remove(at);
            self.reply(target, -Errno::INTR.raw_os_error(), &[])?;
        }
        Ok(())
    }

    fn ioctl(&mut self, request: &Request<'_>) -> io::Result<()> {
        let body = request.body;
        let (Some(command), Some(argument), Some(in_size), Some(out_size)) = (
            u32_at(body, 12),
            u64_at(body, 16),
            u32_at(body, 24),
            u32_at(body, 28),
        ) else {
            return self.reply(request.unique, -Errno::INVAL.raw_os_error(), &[]);
        };
        let input = body.get(32..32 + in_size as usize).unwrap_or_default();
        let call = Ioctl {
            unique: request.unique,
            argument,
            input,
            out_size: out_size as usize,
        };
        match command {
            TCGETS => {
                let termios = self.termios;
                self.ioctl_out(&call, &termios[..TERMIOS])
            }
            TCGETS2 => {
                let termios = self.termios;
                self.ioctl_out(&call, &termios)
            }
            TCSETS | TCSETSW | TCSETSF => self.ioctl_in(&call, TERMIOS, |server, data| {
                server.termios[..TERMIOS].copy_from_slice(data);
            }),
            TCSETS2 | TCSETSW2 | TCSETSF2 => self.ioctl_in(&call, TERMIOS2, |server, data| {
                server.termios.copy_from_slice(data);
            }),
            TIOCMGET => self.ioctl_out(&call, &self.modem_status().to_ne_bytes()),
            TIOCMSET | TIOCMBIS | TIOCMBIC => self.ioctl_in(&call, 4, |server, data| {
                let bits = u32_at(data, 0).unwrap_or(0) & (TIOCM_DTR | TIOCM_RTS);
                server.computer_lines = match command {
                    TIOCMSET => bits,
                    TIOCMBIS => server.computer_lines | bits,
                    _ => server.computer_lines & !bits,
                };
            }),
            FIONREAD => {
                let queued = u32::try_from(self.to_computer.len()).unwrap_or(u32::MAX);
                self.ioctl_out(&call, &queued.to_ne_bytes())
            }
            TIOCOUTQ => self.ioctl_out(&call, &0u32.to_ne_bytes()),
            TCFLSH => {
                if argument != 1 {
                    self.to_computer.clear();
                }
                if argument != 0 {
                    self.to_modem.clear();
                }
                self.ioctl_done(&call)
            }
            TCSBRK | TCXONC | TIOCSBRK | TIOCCBRK | TIOCEXCL | TIOCNXCL => self.ioctl_done(&call),
            other => {
                debug!(command = format!("{other:#x}"), "unsupported ioctl");
                self.reply(request.unique, -Errno::NOTTY.raw_os_error(), &[])
            }
        }
    }

    fn modem_status(&self) -> u32 {
        let mut status = TIOCM_CTS | TIOCM_DSR | self.computer_lines;
        if self.lines.carrier {
            status |= TIOCM_CAR;
        }
        if self.lines.ring {
            status |= TIOCM_RNG;
        }
        status
    }

    fn ioctl_done(&self, call: &Ioctl<'_>) -> io::Result<()> {
        self.reply(call.unique, 0, &[&[0; 16]])
    }

    // Unrestricted ioctls arrive without their data; a retry asks the kernel for it.
    fn ioctl_out(&self, call: &Ioctl<'_>, data: &[u8]) -> io::Result<()> {
        if call.out_size < data.len() {
            return self.retry(call.unique, &[], &[(call.argument, data.len())]);
        }
        self.reply(call.unique, 0, &[&[0; 16], data])
    }

    fn ioctl_in(
        &mut self,
        call: &Ioctl<'_>,
        size: usize,
        apply: impl FnOnce(&mut Self, &[u8]),
    ) -> io::Result<()> {
        if call.input.len() < size {
            return self.retry(call.unique, &[(call.argument, size)], &[]);
        }
        apply(self, &call.input[..size]);
        self.ioctl_done(call)
    }

    fn retry(
        &self,
        unique: u64,
        inputs: &[(u64, usize)],
        outputs: &[(u64, usize)],
    ) -> io::Result<()> {
        let mut out = Vec::new();
        out.extend(0i32.to_ne_bytes());
        out.extend(FUSE_IOCTL_RETRY.to_ne_bytes());
        out.extend(u32::try_from(inputs.len()).unwrap_or(0).to_ne_bytes());
        out.extend(u32::try_from(outputs.len()).unwrap_or(0).to_ne_bytes());
        for &(base, len) in inputs.iter().chain(outputs) {
            out.extend(base.to_ne_bytes());
            out.extend((len as u64).to_ne_bytes());
        }
        self.reply(unique, 0, &[&out])
    }

    fn serve_waiting_reads(&mut self) -> io::Result<()> {
        while !self.to_computer.is_empty() {
            let Some((unique, size)) = self.waiting_reads.pop_front() else {
                break;
            };
            let data = self.take(size);
            self.reply(unique, 0, &[&data])?;
        }
        Ok(())
    }

    fn wake_poll(&mut self) -> io::Result<()> {
        let Some(handle) = self.poll_handle.take() else {
            return Ok(());
        };
        let mut message = Vec::with_capacity(24);
        message.extend(24u32.to_ne_bytes());
        message.extend(FUSE_NOTIFY_POLL.to_ne_bytes());
        message.extend(0u64.to_ne_bytes());
        message.extend(handle.to_ne_bytes());
        self.send(&message)
    }

    fn take(&mut self, size: usize) -> Vec<u8> {
        let n = size.min(self.to_computer.len());
        self.to_computer.drain(..n).collect()
    }

    fn reply(&self, unique: u64, error: i32, parts: &[&[u8]]) -> io::Result<()> {
        let body: usize = parts.iter().map(|part| part.len()).sum();
        let len = u32::try_from(16 + body).unwrap_or(u32::MAX);
        let mut message = Vec::with_capacity(16 + body);
        message.extend(len.to_ne_bytes());
        message.extend(error.to_ne_bytes());
        message.extend(unique.to_ne_bytes());
        for part in parts {
            message.extend_from_slice(part);
        }
        self.send(&message)
    }

    fn send(&self, message: &[u8]) -> io::Result<()> {
        match rustix::io::write(self.fd.get_ref(), message) {
            Ok(_) | Err(Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

async fn next_request(fd: &AsyncFd<OwnedFd>, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut guard = fd.readable().await?;
        match guard.try_io(|fd| Ok(rustix::io::read(fd.get_ref(), &mut *buf)?)) {
            Ok(Ok(n)) => return Ok(n),
            Ok(Err(error)) if error.raw_os_error() == Some(Errno::NOENT.raw_os_error()) => {}
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => {}
        }
    }
}

fn parse_request(message: &[u8]) -> Option<Request<'_>> {
    let len = u32_at(message, 0)? as usize;
    Some(Request {
        opcode: u32_at(message, 4)?,
        unique: u64_at(message, 8)?,
        body: message.get(IN_HEADER..len.min(message.len()))?,
    })
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn default_termios() -> [u8; TERMIOS2] {
    const CS8: u32 = 0o60;
    const CREAD: u32 = 0o200;
    const B115200: u32 = 0o010_002;
    const VMIN: usize = 6;
    let mut termios = [0; TERMIOS2];
    termios[8..12].copy_from_slice(&(CS8 | CREAD | B115200).to_ne_bytes());
    termios[17 + VMIN] = 1;
    termios[36..40].copy_from_slice(&115_200u32.to_ne_bytes());
    termios[40..44].copy_from_slice(&115_200u32.to_ne_bytes());
    termios
}
