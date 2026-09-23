use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_terminal::port::SerialPort;
use softmodem_terminal::settings::Settings;
use softmodem_transport::loopback::{self, Loopback};
use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf, duplex,
};
use tokio::time::{sleep, timeout};

const PATIENCE: Duration = Duration::from_secs(120);
const PAST_GUARD: Duration = Duration::from_millis(1100);

type DcdHistory = Arc<Mutex<Vec<bool>>>;

struct Port {
    stream: DuplexStream,
    dcd: DcdHistory,
    ri: DcdHistory,
}

impl AsyncRead for Port {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for Port {
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

impl SerialPort for Port {
    fn set_carrier(&mut self, on: bool) -> io::Result<()> {
        self.dcd.lock().unwrap().push(on);
        Ok(())
    }

    fn set_ring(&mut self, on: bool) -> io::Result<()> {
        self.ri.lock().unwrap().push(on);
        Ok(())
    }
}

struct Computer {
    port: DuplexStream,
    seen: Vec<u8>,
    cursor: usize,
    dcd: DcdHistory,
    ri: DcdHistory,
}

impl Computer {
    async fn send(&mut self, bytes: &[u8]) {
        self.port.write_all(bytes).await.unwrap();
    }

    async fn read_more(&mut self) {
        let mut buf = [0; 256];
        let n = self.port.read(&mut buf).await.unwrap();
        assert!(n > 0, "the modem closed the serial port");
        self.seen.extend_from_slice(&buf[..n]);
    }

    fn unread(&self) -> String {
        String::from_utf8_lossy(&self.seen[self.cursor..]).into_owned()
    }

    /// Waits until `text` arrives, skipping whatever comes before it.
    async fn expect(&mut self, text: &str) {
        let found = timeout(PATIENCE, async {
            loop {
                if let Some(at) = find(&self.seen[self.cursor..], text.as_bytes()) {
                    self.cursor += at + text.len();
                    return;
                }
                self.read_more().await;
            }
        })
        .await;
        assert!(found.is_ok(), "never saw {text:?}, saw {:?}", self.unread());
    }

    /// Waits for exactly `bytes` as the next thing the modem sends.
    async fn expect_next(&mut self, bytes: &[u8]) {
        let found = timeout(PATIENCE, async {
            while self.seen.len() - self.cursor < bytes.len() {
                self.read_more().await;
            }
        })
        .await;
        assert!(
            found.is_ok(),
            "never saw {bytes:?}, saw {:?}",
            self.unread()
        );
        assert_eq!(
            &self.seen[self.cursor..self.cursor + bytes.len()],
            bytes,
            "unexpected reply"
        );
        self.cursor += bytes.len();
    }

    async fn command(&mut self, line: &str) {
        self.send(format!("{line}\r").as_bytes()).await;
    }

    async fn escape(&mut self) {
        sleep(PAST_GUARD).await;
        self.send(b"+++").await;
        sleep(PAST_GUARD).await;
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn attach(transport: Loopback, settings: Settings) -> Computer {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let (computer, modem_side) = duplex(4096);
    let dcd = DcdHistory::default();
    let ri = DcdHistory::default();
    let port = Port {
        stream: modem_side,
        dcd: dcd.clone(),
        ri: ri.clone(),
    };
    let modem = Modem::new(transport, port, settings, |call, _| call);
    tokio::spawn(async move { modem.run(std::future::pending()).await.unwrap() });
    Computer {
        port: computer,
        seen: Vec::new(),
        cursor: 0,
        dcd,
        ri,
    }
}

fn two_modems(caller: &str, answerer: &str) -> (Computer, Computer) {
    let (a, b) = loopback::pair();
    (
        attach(a, profile(caller).unwrap()),
        attach(b, profile(answerer).unwrap()),
    )
}

#[tokio::test(start_paused = true)]
async fn answers_commands_with_echo_and_ok() {
    let (mut a, _b) = two_modems("", "");
    a.command("AT").await;
    a.expect_next(b"AT\r\r\nOK\r\n").await;
    a.command("at&c1&d2").await;
    a.expect("\r\nOK\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn follows_echo_verbosity_and_quiet_settings() {
    let (mut a, _b) = two_modems("ATE0", "");
    a.command("AT").await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATV0").await;
    a.expect_next(b"0\r").await;
    a.command("ATG").await;
    a.expect_next(b"4\r").await;
    a.command("ATV1Q1").await;
    a.command("AT").await;
    a.command("ATQ0").await;
    a.expect_next(b"\r\nOK\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn reads_writes_and_resets_registers() {
    let (mut a, _b) = two_modems("ATE0", "");
    a.command("ATS7?").await;
    a.expect_next(b"\r\n050\r\n\r\nOK\r\n").await;
    a.command("ATS7=30S7?").await;
    a.expect_next(b"\r\n030\r\n\r\nOK\r\n").await;
    a.command("ATZ").await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATS7?").await;
    a.expect_next(b"\r\n050\r\n\r\nOK\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn identifies_itself() {
    let (mut a, _b) = two_modems("ATE0", "");
    a.command("ATI3").await;
    a.expect_next(format!("\r\nsoftmodem {}\r\n\r\nOK\r\n", env!("CARGO_PKG_VERSION")).as_bytes())
        .await;
    a.command("ATI4").await;
    a.expect_next(b"\r\nV.21 300 bit/s\r\n\r\nOK\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn repeats_the_last_line() {
    let (mut a, _b) = two_modems("ATE0", "");
    a.command("ATS7?").await;
    a.expect_next(b"\r\n050\r\n\r\nOK\r\n").await;
    a.send(b"A/").await;
    a.expect_next(b"\r\n050\r\n\r\nOK\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn a_call_rings_is_answered_and_carries_data_both_ways() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0");
    a.command("ATDT0300").await;
    b.expect("RING").await;
    b.command("ATA").await;
    a.expect("CONNECT").await;
    b.expect("CONNECT").await;

    a.send(b"hello from the caller").await;
    b.expect("hello from the caller").await;
    b.send(b"hello from the answerer").await;
    a.expect("hello from the answerer").await;
}

#[tokio::test(start_paused = true)]
async fn keeps_what_the_answerer_sends_before_the_caller_connects() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDT0300").await;
    b.expect("CONNECT").await;
    b.send(b"sent the moment the answerer connected").await;
    a.expect("CONNECT").await;
    a.expect("sent the moment the answerer connected").await;
}

#[tokio::test(start_paused = true)]
async fn escapes_to_command_mode_and_back_and_hangs_up() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDT0300").await;
    a.expect("CONNECT\r\n").await;
    b.expect("CONNECT").await;

    a.escape().await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATO").await;
    a.expect_next(b"\r\nCONNECT\r\n").await;
    a.send(b"still here").await;
    b.expect("still here").await;

    a.escape().await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATH").await;
    a.expect_next(b"\r\nOK\r\n").await;
    b.expect("NO CARRIER").await;
}

#[tokio::test(start_paused = true)]
async fn pluses_inside_data_are_sent() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDT0300").await;
    a.expect("CONNECT").await;
    b.expect("CONNECT").await;
    a.send(b"1+++2").await;
    b.expect("1+++2").await;
}

#[tokio::test(start_paused = true)]
async fn auto_answers_after_the_rings_in_s0() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=2");
    a.command("ATDT0300").await;
    b.expect_next(b"\r\nRING\r\n").await;
    let first_ring = tokio::time::Instant::now();
    b.expect_next(b"\r\nRING\r\n").await;
    assert!(first_ring.elapsed() >= Duration::from_secs(5));
    b.expect("CONNECT").await;
    a.expect("CONNECT").await;
}

#[tokio::test(start_paused = true)]
async fn an_off_hook_modem_is_busy() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0");
    b.command("ATH1").await;
    b.expect("OK").await;
    a.command("ATDT0300").await;
    a.expect_next(b"\r\nBUSY\r\n").await;
    a.command("ATX0DL").await;
    a.expect_next(b"\r\nNO CARRIER\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn gives_up_after_s7_without_an_answer() {
    let (mut a, mut b) = two_modems("ATE0S7=10", "ATE0");
    a.command("ATDT0300").await;
    b.expect("RING").await;
    let dialled = tokio::time::Instant::now();
    a.expect_next(b"\r\nNO CARRIER\r\n").await;
    assert!(dialled.elapsed() <= Duration::from_secs(11));
}

#[tokio::test(start_paused = true)]
async fn a_key_press_aborts_the_dial() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0");
    a.command("ATDT0300").await;
    b.expect("RING").await;
    a.send(b"x").await;
    a.expect_next(b"\r\nNO CARRIER\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn redials_the_last_number() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDL").await;
    a.expect_next(b"\r\nNO CARRIER\r\n").await;
    a.command("ATDT0300").await;
    a.expect("CONNECT").await;
    b.expect("CONNECT").await;
    a.escape().await;
    a.command("ATH").await;
    a.expect("OK").await;
    b.expect("NO CARRIER").await;
    a.command("ATDL").await;
    a.expect("CONNECT").await;
}

#[tokio::test(start_paused = true)]
async fn a_semicolon_places_the_call_without_a_handshake() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDT0300;").await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATO").await;
    a.expect("CONNECT").await;
    b.expect("CONNECT").await;
    a.send(b"late start").await;
    b.expect("late start").await;
}

impl Computer {
    fn dcd(&self) -> Vec<bool> {
        self.dcd.lock().unwrap().clone()
    }

    fn ri(&self) -> Vec<bool> {
        self.ri.lock().unwrap().clone()
    }
}

#[tokio::test(start_paused = true)]
async fn ri_rises_with_each_ring_and_falls_on_answer() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0");
    a.command("ATDT0300").await;
    b.expect("RING").await;
    assert_eq!(b.ri(), [true]);
    sleep(Duration::from_secs(3)).await;
    assert_eq!(b.ri(), [true, false]);
    b.expect("RING").await;
    b.command("ATA").await;
    b.expect("CONNECT").await;
    assert_eq!(b.ri(), [true, false, true, false]);
}

#[tokio::test(start_paused = true)]
async fn dcd_stays_on_under_c0() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDT0300").await;
    a.expect("CONNECT").await;
    b.expect("CONNECT").await;
    a.escape().await;
    a.command("ATH").await;
    a.expect("OK").await;
    b.expect("NO CARRIER").await;
    assert_eq!(a.dcd(), [true]);
    assert_eq!(b.dcd(), [true]);
}

#[tokio::test(start_paused = true)]
async fn dcd_follows_carrier_under_c1() {
    let (mut a, mut b) = two_modems("ATE0&C1", "ATE0&C1S0=1");
    a.command("AT").await;
    a.expect("OK").await;
    assert!(a.dcd().is_empty(), "DCD rose before a call");

    a.command("ATDT0300").await;
    a.expect("CONNECT").await;
    b.expect("CONNECT").await;
    assert_eq!(a.dcd(), [true]);
    assert_eq!(b.dcd(), [true]);

    a.escape().await;
    a.command("ATH").await;
    a.expect("OK").await;
    b.expect("NO CARRIER").await;
    sleep(Duration::from_secs(1)).await;
    assert_eq!(a.dcd(), [true, false]);
    assert_eq!(b.dcd(), [true, false]);
}

#[tokio::test(start_paused = true)]
async fn dcd_follows_a_change_of_c_at_once() {
    let (mut a, _b) = two_modems("ATE0&C1", "");
    a.command("AT&C0").await;
    a.expect("OK").await;
    assert_eq!(a.dcd(), [true]);
    a.command("AT&F&C1").await;
    a.expect("OK").await;
    sleep(Duration::from_secs(1)).await;
    assert_eq!(a.dcd(), [true, false]);
}

#[tokio::test(start_paused = true)]
async fn a_bad_profile_is_refused() {
    assert!(profile("ATDT0300").is_err());
    assert!(profile("ATX9").is_err());
    assert_eq!(profile("").unwrap(), Settings::default());
}
