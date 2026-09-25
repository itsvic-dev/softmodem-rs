// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Calls through a real SIP registrar. Ignored unless asked for, and then
//! needs `SOFTMODEM_PBX`, `USER1`, `USER1_PASS`, `USER2` and `USER2_PASS`.
//! With `SOFTMODEM_PBX_UDP` set, SIP goes over UDP, not TCP.

use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_terminal::port::Plain;
use softmodem_transport::sip::{Account, Protocol, Sip};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};
use tokio::time::{sleep, timeout};

const PATIENCE: Duration = Duration::from_secs(70);
const PAST_GUARD: Duration = Duration::from_millis(1100);

fn var(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
}

struct Computer {
    port: DuplexStream,
    seen: Vec<u8>,
}

impl Computer {
    async fn command(&mut self, line: &str) {
        self.port
            .write_all(format!("{line}\r").as_bytes())
            .await
            .unwrap();
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.port.write_all(bytes).await.unwrap();
    }

    async fn expect(&mut self, text: &str) {
        let found = timeout(PATIENCE, async {
            loop {
                if let Some(at) = self
                    .seen
                    .windows(text.len())
                    .position(|window| window == text.as_bytes())
                {
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
            "never saw {text:?}, saw {:?}",
            String::from_utf8_lossy(&self.seen)
        );
    }
}

async fn modem(user: &str, password: &str, init: &str) -> Computer {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let sip = Sip::register(Account {
        registrar: var("SOFTMODEM_PBX"),
        user: var(user),
        password: var(password),
        protocol: if std::env::var_os("SOFTMODEM_PBX_UDP").is_some() {
            Protocol::Udp
        } else {
            Protocol::Tcp
        },
    })
    .await
    .expect("registering");
    let (computer, modem_side) = duplex(4096);
    let (input, output) = tokio::io::split(modem_side);
    let modem = Modem::new(
        sip,
        Plain { input, output },
        profile(init).unwrap(),
        |call, _| call,
    );
    tokio::spawn(async move { Box::pin(modem.run(std::future::pending())).await.unwrap() });
    Computer {
        port: computer,
        seen: Vec::new(),
    }
}

#[tokio::test]
#[ignore = "needs a live SIP registrar and two accounts"]
async fn a_call_through_the_pbx_carries_data_and_hangs_up() {
    let mut caller = modem("USER1", "USER1_PASS", "ATE0").await;
    let mut answerer = modem("USER2", "USER2_PASS", "ATE0S0=1").await;

    caller.command(&format!("ATDT{}", var("USER2"))).await;
    caller.expect("CONNECT").await;
    answerer.expect("CONNECT").await;

    caller.send(b"to the answerer").await;
    answerer.expect("to the answerer").await;
    answerer.send(b"to the caller").await;
    caller.expect("to the caller").await;

    sleep(PAST_GUARD).await;
    caller.send(b"+++").await;
    sleep(PAST_GUARD).await;
    caller.expect("OK").await;
    caller.command("ATH").await;
    caller.expect("OK").await;
    answerer.expect("NO CARRIER").await;
}

#[tokio::test]
#[ignore = "needs a live SIP registrar and two accounts"]
async fn a_modem_in_a_call_is_busy_to_another_caller() {
    let mut caller = modem("USER1", "USER1_PASS", "ATE0").await;
    let mut answerer = modem("USER2", "USER2_PASS", "ATE0S0=1").await;
    caller.command(&format!("ATDT{}", var("USER2"))).await;
    caller.expect("CONNECT").await;
    answerer.expect("CONNECT").await;

    let mut second = modem("USER1", "USER1_PASS", "ATE0").await;
    second.command(&format!("ATDT{}", var("USER2"))).await;
    second.expect("BUSY").await;

    caller.send(b"still connected").await;
    answerer.expect("still connected").await;
}

#[tokio::test]
#[ignore = "needs a live SIP registrar and two accounts"]
async fn a_dial_given_up_stops_the_ringing() {
    let mut caller = modem("USER1", "USER1_PASS", "ATE0").await;
    let mut answerer = modem("USER2", "USER2_PASS", "ATE0").await;
    caller.command(&format!("ATDT{}", var("USER2"))).await;
    answerer.expect("RING").await;
    caller.send(b"x").await;
    caller.expect("NO CARRIER").await;

    sleep(Duration::from_secs(1)).await;
    answerer.seen.clear();
    let rang_again = timeout(Duration::from_secs(13), answerer.expect("RING")).await;
    assert!(rang_again.is_err(), "the answerer kept ringing");
}

#[tokio::test]
#[ignore = "needs a live SIP registrar and two accounts"]
async fn an_off_hook_modem_is_busy_through_the_pbx() {
    let mut caller = modem("USER1", "USER1_PASS", "ATE0").await;
    let mut answerer = modem("USER2", "USER2_PASS", "ATE0").await;
    answerer.command("ATH1").await;
    answerer.expect("OK").await;

    caller.command(&format!("ATDT{}", var("USER2"))).await;
    caller.expect("BUSY").await;
}
