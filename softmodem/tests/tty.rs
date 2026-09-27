// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;
use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_terminal::pty::Pty;
use softmodem_terminal::tty::TtyPort;
use softmodem_transport::wire::{Impairment, Wire};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

const LOOPBACK: &str = "127.0.0.1:0";
const PATIENCE: Duration = Duration::from_secs(40);

async fn read_until(port: &mut (impl AsyncRead + Unpin), marker: &[u8]) {
    let mut seen = Vec::new();
    let found = timeout(PATIENCE, async {
        while !seen.ends_with(marker) {
            seen.push(port.read_u8().await.unwrap());
        }
    })
    .await;
    assert!(
        found.is_ok(),
        "never saw {:?}, saw {:?}",
        String::from_utf8_lossy(marker),
        String::from_utf8_lossy(&seen)
    );
}

fn serve(wire: Wire, port: TtyPort, init: &str) {
    let profile = profile(init).unwrap();
    tokio::spawn(async move {
        Modem::new(wire, port, profile, |call, _| (call, None))
            .run(std::future::pending())
            .await
            .unwrap();
    });
}

fn link(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("softmodem-tty-{}-{name}", std::process::id()))
}

#[tokio::test]
async fn a_hang_up_of_the_device_ends_the_call_and_the_reopened_device_gets_the_modem() {
    let answering = Wire::bind(LOOPBACK.parse().unwrap(), None, Impairment::default())
        .await
        .unwrap();
    let peer = answering.local_addr().unwrap();
    let calling = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), Impairment::default())
        .await
        .unwrap();
    let caller_link = link("caller");
    let isp_link = link("isp");
    let mut caller = Pty::open(Some(&caller_link)).unwrap();
    let mut isp = Pty::open(Some(&isp_link)).unwrap();
    serve(calling, TtyPort::open(&caller_link, 9600).unwrap(), "ATE0");
    serve(
        answering,
        TtyPort::open(&isp_link, 9600).unwrap(),
        "ATE0S0=1",
    );

    caller.write_all(b"ATDT0300\r").await.unwrap();
    read_until(&mut caller, b"CONNECT").await;
    read_until(&mut isp, b"CONNECT").await;

    caller.hang_up().unwrap();
    read_until(&mut isp, b"NO CARRIER").await;

    caller.write_all(b"AT\r").await.unwrap();
    read_until(&mut caller, b"OK\r\n").await;
}
