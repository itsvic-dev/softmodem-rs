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
    speaker: Arc<Mutex<Vec<f32>>>,
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
    let speaker = Arc::<Mutex<Vec<f32>>>::default();
    let gains = speaker.clone();
    let modem = Modem::new(transport, port, settings, |call, _| call)
        .with_speaker(move |gain| gains.lock().unwrap().push(gain));
    tokio::spawn(async move { modem.run(std::future::pending()).await.unwrap() });
    Computer {
        port: computer,
        seen: Vec::new(),
        cursor: 0,
        dcd,
        ri,
        speaker,
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
    a.expect_next(
        b"\r\nV.21 300 bit/s, V.22 1200 bit/s, V.22bis 2400 bit/s, V.34 33600 bit/s, V.42 LAPM, V.42bis\r\n\r\nOK\r\n",
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn reads_and_lists_the_error_control() {
    let (mut a, _b) = two_modems("ATE0\\N2", "");
    a.command("AT+ES?;+ER?").await;
    a.expect_next(b"\r\n+ES: 3,3,5\r\n\r\n+ER: 0\r\n\r\nOK\r\n")
        .await;
    a.command("AT+ES=,1;+ER=1;+ES?;+ER?").await;
    a.expect_next(b"\r\n+ES: 3,1,5\r\n\r\n+ER: 1\r\n\r\nOK\r\n")
        .await;
    a.command("AT&F+ES?").await;
    a.expect_next(b"\r\n+ES: 3,0,2\r\n\r\nOK\r\n").await;
    a.command("AT+ES=?;+ER=?").await;
    a.expect_next(b"AT+ES=?;+ER=?\r\r\n+ES: (0-3),(0-3),(0-5)\r\n\r\n+ER: (0,1)\r\n\r\nOK\r\n")
        .await;
    a.command("AT+ES=4").await;
    a.expect_next(b"AT+ES=4\r\r\nERROR\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn two_modems_connect_with_v42_and_carry_data_both_ways() {
    let (mut a, mut b) = two_modems("ATE0+ER=1", "ATE0S0=1+ER=1");
    a.command("ATDT0300").await;
    a.expect("\r\n+ER: LAPM\r\n\r\nCONNECT 33600\r\n").await;
    b.expect("\r\n+ER: LAPM\r\n\r\nCONNECT 33600\r\n").await;

    let text = (0..300)
        .map(|n| format!("line {n} from the caller\r\n"))
        .collect::<Vec<_>>()
        .concat();
    a.send(text.as_bytes()).await;
    b.send(b"hello from the answerer").await;
    b.expect(&text).await;
    a.expect("hello from the answerer").await;
}

#[tokio::test(start_paused = true)]
async fn falls_back_to_plain_data_when_one_end_has_no_v42() {
    for (caller, answerer) in [("\\N0", ""), ("", "\\N0"), ("+ES=1", "+ES=,,2")] {
        let (mut a, mut b) = two_modems(
            &format!("ATE0+ER=1;{caller}"),
            &format!("ATE0S0=1+ER=1;{answerer}"),
        );
        a.command("ATDT0300").await;
        a.expect("\r\n+ER: NONE\r\n\r\nCONNECT 33600\r\n").await;
        b.expect("\r\n+ER: NONE\r\n\r\nCONNECT 33600\r\n").await;
        a.send(b"plain from the caller").await;
        b.expect("plain from the caller").await;
        b.send(b"plain from the answerer").await;
        a.expect("plain from the answerer").await;
    }
}

#[tokio::test(start_paused = true)]
async fn hangs_up_when_v42_is_required_and_the_far_end_has_none() {
    let (mut a, _b) = two_modems("ATE0\\N2", "ATE0S0=1\\N0");
    a.command("ATDT0300").await;
    a.expect_next(b"\r\nNO CARRIER\r\n").await;
    let (mut a, mut b) = two_modems("ATE0\\N0", "ATE0S0=1\\N2");
    a.command("ATDT0300").await;
    b.expect("\r\nNO CARRIER\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn v21_stays_plain() {
    let (mut a, mut b) = two_modems("ATE0+ER=1;+MS=V21", "ATE0S0=1+ER=1;+MS=V21");
    a.command("ATDT0300").await;
    a.expect("\r\n+ER: NONE\r\n\r\nCONNECT\r\n").await;
    b.expect("\r\n+ER: NONE\r\n\r\nCONNECT\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn reads_and_lists_the_compression() {
    let (mut a, _b) = two_modems("ATE0%C0", "");
    a.command("AT+DS?;+DR?").await;
    a.expect_next(b"\r\n+DS: 0,0,2048,32\r\n\r\n+DR: 0\r\n\r\nOK\r\n")
        .await;
    a.command("AT+DS=3,1,4096;+DR=1;+DS?;+DR?").await;
    a.expect_next(b"\r\n+DS: 3,1,4096,32\r\n\r\n+DR: 1\r\n\r\nOK\r\n")
        .await;
    a.command("AT+DS=?;+DR=?").await;
    a.expect_next(b"\r\n+DS: (0-3),(0,1),(512-65535),(6-250)\r\n\r\n+DR: (0,1)\r\n\r\nOK\r\n")
        .await;
}

fn compressible(lines: usize) -> String {
    (0..lines)
        .map(|n| format!("line {n} of the text the caller sends to the answerer\r\n"))
        .collect::<Vec<_>>()
        .concat()
}

#[tokio::test(start_paused = true)]
async fn two_modems_compress_with_v42bis_both_ways() {
    let (mut a, mut b) = two_modems("ATE0+ER=1;+DR=1", "ATE0S0=1+ER=1;+DR=1");
    a.command("ATDT0300").await;
    a.expect("\r\n+ER: LAPM\r\n\r\n+DR: V42B\r\n\r\nCONNECT 33600\r\n")
        .await;
    b.expect("\r\n+ER: LAPM\r\n\r\n+DR: V42B\r\n\r\nCONNECT 33600\r\n")
        .await;
    let text = compressible(200);
    tokio::join!(a.send(text.as_bytes()), b.expect(&text));
    tokio::join!(b.send(text.as_bytes()), a.expect(&text));
}

#[tokio::test(start_paused = true)]
async fn reports_compression_in_one_direction_or_none() {
    let (mut a, mut b) = two_modems("ATE0+DR=1;+DS=2", "ATE0S0=1+DR=1");
    a.command("ATDT0300").await;
    a.expect("\r\n+DR: V42B RD\r\n\r\nCONNECT 33600\r\n").await;
    b.expect("\r\n+DR: V42B TD\r\n\r\nCONNECT 33600\r\n").await;
    b.send(b"compressed only this way").await;
    a.expect("compressed only this way").await;
    a.send(b"and plain this way").await;
    b.expect("and plain this way").await;

    let (mut a, mut b) = two_modems("ATE0+DR=1", "ATE0S0=1+DR=1;%C0");
    a.command("ATDT0300").await;
    a.expect("\r\n+DR: NONE\r\n\r\nCONNECT 33600\r\n").await;
    b.expect("\r\n+DR: NONE\r\n\r\nCONNECT 33600\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn hangs_up_when_v42bis_is_required_and_the_far_end_has_none() {
    let (mut a, _b) = two_modems("ATE0+DS=3,1", "ATE0S0=1%C0");
    a.command("ATDT0300").await;
    a.expect_next(b"\r\nNO CARRIER\r\n").await;
}

async fn transfer_time(caller: &str, answerer: &str, text: &str) -> Duration {
    let (mut a, mut b) = two_modems(caller, answerer);
    a.command("ATDT0300").await;
    a.expect("CONNECT 33600\r\n").await;
    b.expect("CONNECT 33600\r\n").await;
    let start = tokio::time::Instant::now();
    tokio::join!(a.send(text.as_bytes()), b.expect(text));
    start.elapsed()
}

#[tokio::test(start_paused = true)]
async fn v42bis_carries_text_faster_than_the_line_rate() {
    let text = compressible(400);
    let compressed = transfer_time("ATE0", "ATE0S0=1", &text).await;
    let plain = transfer_time("ATE0%C0", "ATE0S0=1%C0", &text).await;
    let line_rate = Duration::from_millis(u64::try_from(text.len()).unwrap() * 10_000 / 33_600);
    assert!(
        compressed * 2 < plain && compressed < line_rate,
        "{} octets: {compressed:?} with V.42bis, {plain:?} without, {line_rate:?} at 33600 bit/s async",
        text.len()
    );
}

#[tokio::test(start_paused = true)]
async fn reads_and_lists_the_modulation() {
    let (mut a, _b) = two_modems("ATE0+MS=V22,0", "");
    a.command("AT+MS?").await;
    a.expect_next(b"\r\n+MS: V22,0\r\n\r\nOK\r\n").await;
    a.command("AT+MS=V21;+MS?").await;
    a.expect_next(b"\r\n+MS: V21,1\r\n\r\nOK\r\n").await;
    a.command("ATZ+MS?").await;
    a.expect_next(b"\r\n+MS: V22,0\r\n\r\nOK\r\n").await;
    a.command("AT&FE0+MS?").await;
    a.expect_next(b"\r\n+MS: V34,1\r\n\r\nOK\r\n").await;
    a.command("AT+MS=?").await;
    a.expect_next(b"\r\n+MS: (V21,V22,V22B,V34,V90),(0,1)\r\n\r\nOK\r\n")
        .await;
    a.command("AT+MS=V22,2").await;
    a.expect_next(b"\r\nERROR\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn a_v22_call_connects_at_1200_and_carries_data_both_ways() {
    let (mut a, mut b) = two_modems("ATE0+MS=V22", "ATE0S0=1+MS=V22");
    a.command("ATDT0300").await;
    a.expect("CONNECT 1200\r\n").await;
    b.expect("CONNECT 1200\r\n").await;

    let text = (0..20)
        .map(|n| format!("line {n} from the caller\r\n"))
        .collect::<Vec<_>>()
        .concat();
    a.send(text.as_bytes()).await;
    b.expect(&text).await;
    b.send(b"hello from the answerer").await;
    a.expect("hello from the answerer").await;

    a.escape().await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATO").await;
    a.expect_next(b"\r\nCONNECT 1200\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn a_v22bis_call_connects_at_2400_and_carries_data_both_ways() {
    let (mut a, mut b) = two_modems("ATE0+MS=V22B", "ATE0S0=1+MS=V22B");
    a.command("ATDT0300").await;
    a.expect("CONNECT 2400\r\n").await;
    b.expect("CONNECT 2400\r\n").await;

    let text = (0..40)
        .map(|n| format!("line {n} from the caller\r\n"))
        .collect::<Vec<_>>()
        .concat();
    a.send(text.as_bytes()).await;
    b.expect(&text).await;
    b.send(b"hello from the answerer").await;
    a.expect("hello from the answerer").await;
}

#[tokio::test(start_paused = true)]
async fn ato1_retrains_and_the_call_goes_on() {
    for (modulation, connect) in [
        ("+MS=V22B", "CONNECT 2400\r\n"),
        ("+MS=V34", "CONNECT 33600\r\n"),
    ] {
        let (mut a, mut b) = two_modems(
            &format!("ATE0{modulation}"),
            &format!("ATE0S0=1{modulation}"),
        );
        a.command("ATDT0300").await;
        a.expect(connect).await;
        b.expect(connect).await;

        a.escape().await;
        a.expect_next(b"\r\nOK\r\n").await;
        a.command("ATO1").await;
        a.expect_next(format!("\r\n{connect}").as_bytes()).await;
        sleep(Duration::from_secs(8)).await;
        a.send(b"after the retrain").await;
        b.expect("after the retrain").await;
        b.send(b"and back").await;
        a.expect("and back").await;
    }
}

#[tokio::test(start_paused = true)]
async fn v22bis_falls_back_to_a_v22_modem() {
    let (mut a, mut b) = two_modems("ATE0+MS=V22B", "ATE0S0=1+MS=V22");
    a.command("ATDT0300").await;
    a.expect("CONNECT 1200\r\n").await;
    b.expect("CONNECT 1200\r\n").await;
    a.send(b"hello at 1200").await;
    b.expect("hello at 1200").await;
}

#[tokio::test(start_paused = true)]
async fn a_fixed_v22_does_not_connect_to_a_fixed_v21() {
    let (mut a, mut b) = two_modems("ATE0S7=10+MS=V22,0", "ATE0S0=1S7=10+MS=V21,0");
    a.command("ATDT0300").await;
    a.expect("NO CARRIER").await;
    b.expect("NO CARRIER").await;
}

#[tokio::test(start_paused = true)]
async fn automode_meets_each_fixed_modulation_in_both_roles() {
    let mut calls = Vec::new();
    for (fixed, connect) in [
        ("+MS=V21,0", "CONNECT\r\n"),
        ("+MS=V22,0", "CONNECT 1200\r\n"),
        ("+MS=V22B,0", "CONNECT 2400\r\n"),
        ("+MS=V34,0", "CONNECT 33600\r\n"),
    ] {
        for automode_answers in [true, false] {
            let (caller, answerer) = if automode_answers {
                (format!("ATE0{fixed}"), "ATE0S0=1".to_string())
            } else {
                ("ATE0".to_string(), format!("ATE0S0=1{fixed}"))
            };
            let (mut a, mut b) = two_modems(&caller, &answerer);
            a.command("ATDT0300").await;
            a.expect(connect).await;
            b.expect(connect).await;
            a.send(b"from the caller").await;
            b.expect("from the caller").await;
            b.send(b"from the answerer").await;
            a.expect("from the answerer").await;
            calls.push((a, b));
        }
    }
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
    let mut calls = Vec::new();
    for modulation in [
        "+MS=V21,0",
        "+MS=V22,0",
        "+MS=V22B,0",
        "+MS=V22B,1",
        "+MS=V34,0",
        "+MS=V34,1",
    ] {
        let (mut a, mut b) = two_modems(
            &format!("ATE0{modulation}"),
            &format!("ATE0S0=1{modulation}"),
        );
        a.command("ATDT0300").await;
        b.expect("CONNECT").await;
        b.expect("\r\n").await;
        b.send(b"sent the moment the answerer connected").await;
        a.expect("CONNECT").await;
        a.expect("\r\n").await;
        a.expect_next(b"sent the moment the answerer connected")
            .await;
        calls.push((a, b));
    }
}

#[tokio::test(start_paused = true)]
async fn escapes_to_command_mode_and_back_and_hangs_up() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0S0=1");
    a.command("ATDT0300").await;
    a.expect("CONNECT 33600\r\n").await;
    b.expect("CONNECT").await;

    a.escape().await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATO").await;
    a.expect_next(b"\r\nCONNECT 33600\r\n").await;
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

    fn speaker(&self) -> Vec<f32> {
        self.speaker.lock().unwrap().clone()
    }
}

#[tokio::test(start_paused = true)]
async fn the_speaker_sounds_until_connect_under_m1_and_always_under_m2() {
    let (mut a, mut b) = two_modems("ATE0", "ATE0M2L3S0=1");
    a.command("ATDT0300").await;
    a.expect("CONNECT 33600\r\n").await;
    b.expect("CONNECT").await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(a.speaker(), [0.5, 0.0]);
    assert_eq!(b.speaker(), [1.0]);

    a.escape().await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATH").await;
    a.expect_next(b"\r\nOK\r\n").await;
    a.command("ATM0").await;
    a.expect_next(b"\r\nOK\r\n").await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(a.speaker(), [0.5, 0.0, 0.5, 0.0]);
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
