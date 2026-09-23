//! Whole calls between our modem and a modem built from spandsp's parts, the
//! nearest thing to a real modem that runs in a test.

use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_interop::{AnswerTone, FskChannel, FskRx, FskTx, ToneRx, ToneTx};
use softmodem_terminal::port::Plain;
use softmodem_transport::loopback::{self, Loopback};
use softmodem_transport::{Call, Incoming, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};
use tokio::time::{sleep, timeout};

const FRAME: usize = 160;
const PATIENCE: Duration = Duration::from_secs(120);
const PAST_GUARD: Duration = Duration::from_millis(1100);
const TO_US: &[u8] = b"hello from spandsp";
const FROM_US: &[u8] = b"hello from softmodem";

struct Computer {
    port: DuplexStream,
    seen: Vec<u8>,
}

impl Computer {
    async fn command(&mut self, line: &str) {
        self.send(format!("{line}\r").as_bytes()).await;
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.port.write_all(bytes).await.unwrap();
    }

    async fn expect(&mut self, text: &[u8]) {
        let found = timeout(PATIENCE, async {
            loop {
                if let Some(at) = self.seen.windows(text.len()).position(|w| w == text) {
                    self.seen.drain(..at + text.len());
                    return;
                }
                let mut buf = [0; 256];
                let n = self.port.read(&mut buf).await.unwrap();
                assert!(n > 0, "the modem closed the serial port");
                self.seen.extend_from_slice(&buf[..n]);
            }
        })
        .await;
        assert!(
            found.is_ok(),
            "never saw {:?}, saw {:?}",
            String::from_utf8_lossy(text),
            String::from_utf8_lossy(&self.seen)
        );
    }

    async fn hang_up(&mut self) {
        sleep(PAST_GUARD).await;
        self.send(b"+++").await;
        sleep(PAST_GUARD).await;
        self.expect(b"OK").await;
        self.command("ATH").await;
        self.expect(b"OK").await;
    }
}

fn ours(transport: Loopback, init: &str) -> Computer {
    let (computer, modem_side) = duplex(4096);
    let (input, output) = tokio::io::split(modem_side);
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let modem = Modem::new(
        transport,
        Plain { input, output },
        profile(init).unwrap(),
        |call, _| call,
    );
    tokio::spawn(async move {
        if let Err(error) = modem.run(std::future::pending()).await {
            eprintln!("our modem stopped: {error}");
        }
    });
    Computer {
        port: computer,
        seen: Vec::new(),
    }
}

fn ms(samples: usize) -> usize {
    samples / 8
}

// A V.25 answerer: silence, answer tone, a gap, channel 2, then `TO_US`.
async fn spandsp_answers(mut line: Loopback, tone: AnswerTone) -> Vec<u8> {
    let Incoming::Ringing { caller, .. } = line.incoming().await.unwrap() else {
        panic!("the caller gave up");
    };
    let mut call = line.answer(&caller).await.unwrap();
    let mut answer_tone = ToneTx::new(tone);
    let mut transmitter = FskTx::new(FskChannel::V21Answer);
    let mut receiver = FskRx::new(FskChannel::V21Originate);
    let carrier_at = 16_000 + 26_400 + 600;
    let mut sent = 0;
    let mut heard_carrier = false;
    let mut replied = false;

    exchange(&mut call, |received| {
        receiver.process(received);
        if receiver.carrier() && !heard_carrier {
            heard_carrier = true;
            eprintln!("spandsp answerer: channel 1 carrier at {} ms", ms(sent));
        }
        let mut out = vec![0; FRAME];
        if (16_000..16_000 + 26_400).contains(&sent) {
            answer_tone.render(&mut out);
        } else if sent >= carrier_at {
            if sent >= carrier_at + 8000 && !replied {
                replied = true;
                transmitter.send(TO_US);
            }
            transmitter.render(&mut out);
        }
        sent += FRAME;
        out
    })
    .await;
    receiver.bytes().to_vec()
}

// A V.25 caller: silent until answer tone and channel 2, then channel 1 and `TO_US`.
async fn spandsp_calls(mut line: Loopback) -> Vec<u8> {
    let mut call = line.dial("0300").await.unwrap();
    let mut tone = ToneRx::new(AnswerTone::Ans);
    let mut transmitter = FskTx::new(FskChannel::V21Originate);
    let mut receiver = FskRx::new(FskChannel::V21Answer);
    let mut sent = 0;
    let mut tone_at = None;
    let mut carrier_from = None;
    let mut replied = false;

    exchange(&mut call, |received| {
        tone.process(received);
        receiver.process(received);
        if tone.detected().is_some() && tone_at.is_none() {
            tone_at = Some(sent);
            eprintln!("spandsp caller: answer tone at {} ms", ms(sent));
        }
        if tone_at.is_some() && receiver.carrier() && carrier_from.is_none() {
            carrier_from = Some(sent);
            eprintln!("spandsp caller: channel 2 carrier at {} ms", ms(sent));
        }
        let mut out = vec![0; FRAME];
        if let Some(from) = carrier_from {
            if sent >= from + 8000 && !replied {
                replied = true;
                transmitter.send(TO_US);
            }
            transmitter.render(&mut out);
        }
        sent += FRAME;
        out
    })
    .await;
    receiver.bytes().to_vec()
}

async fn exchange(call: &mut Call, mut respond: impl FnMut(&[i16]) -> Vec<i16>) {
    while let Some(received) = call.audio_in.recv().await {
        let out = respond(&received);
        if call.audio_out.send(out).await.is_err() {
            return;
        }
    }
}

async fn we_call_spandsp(tone: AnswerTone) {
    let (a, b) = loopback::pair();
    let answerer = tokio::spawn(spandsp_answers(b, tone));
    let mut computer = ours(a, "ATE0");
    computer.command("ATDT0300").await;
    computer.expect(b"CONNECT").await;
    computer.expect(TO_US).await;
    computer.send(FROM_US).await;
    sleep(Duration::from_secs(2)).await;
    computer.hang_up().await;
    let heard = timeout(PATIENCE, answerer).await.unwrap().unwrap();
    assert_eq!(heard, FROM_US, "spandsp heard something else");
}

#[tokio::test(start_paused = true)]
async fn we_call_a_v25_modem_that_sends_ans() {
    we_call_spandsp(AnswerTone::Ans).await;
}

#[tokio::test(start_paused = true)]
async fn we_call_a_modem_that_sends_ans_with_phase_reversals() {
    we_call_spandsp(AnswerTone::AnsPr).await;
}

#[tokio::test(start_paused = true)]
async fn we_call_a_v8_modem_that_sends_ansam_with_phase_reversals() {
    we_call_spandsp(AnswerTone::AnsamPr).await;
}

#[tokio::test(start_paused = true)]
async fn a_v25_modem_calls_us() {
    let (a, b) = loopback::pair();
    let mut computer = ours(b, "ATE0S0=1");
    let caller = tokio::spawn(spandsp_calls(a));
    computer.expect(b"CONNECT").await;
    computer.expect(TO_US).await;
    computer.send(FROM_US).await;
    sleep(Duration::from_secs(2)).await;
    computer.hang_up().await;
    let heard = timeout(PATIENCE, caller).await.unwrap().unwrap();
    assert_eq!(heard, FROM_US, "spandsp heard something else");
}
