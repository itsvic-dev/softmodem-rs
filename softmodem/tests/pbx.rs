//! Calls through a real SIP registrar. Ignored unless asked for, and then
//! needs `SOFTMODEM_PBX`, `USER1`, `USER1_PASS`, `USER2` and `USER2_PASS`.

use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_terminal::port::Plain;
use softmodem_transport::sip::{Account, Sip};
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
    let sip = Sip::register(Account {
        registrar: var("SOFTMODEM_PBX"),
        user: var(user),
        password: var(password),
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
async fn an_off_hook_modem_is_busy_through_the_pbx() {
    let mut caller = modem("USER1", "USER1_PASS", "ATE0").await;
    let mut answerer = modem("USER2", "USER2_PASS", "ATE0").await;
    answerer.command("ATH1").await;
    answerer.expect("OK").await;

    caller.command(&format!("ATDT{}", var("USER2"))).await;
    caller.expect("BUSY").await;
}
