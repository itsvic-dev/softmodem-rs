// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::net::SocketAddr;
use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_terminal::tcp::TcpPort;
use softmodem_transport::wire::{Impairment, Wire};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const LOOPBACK: &str = "127.0.0.1:0";
const PATIENCE: Duration = Duration::from_secs(40);

async fn serve(wire: Wire, init: &str) -> SocketAddr {
    let port = TcpPort::bind(LOOPBACK.parse().unwrap()).await.unwrap();
    let address = port.local_addr().unwrap();
    let profile = profile(init).unwrap();
    tokio::spawn(async move {
        Modem::new(wire, port, profile, |call, _| call)
            .run(std::future::pending())
            .await
            .unwrap();
    });
    address
}

async fn read_until(port: &mut TcpStream, marker: &[u8]) {
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

#[tokio::test]
async fn a_computer_leaving_hangs_up_and_the_next_one_gets_the_modem() {
    let answering = Wire::bind(LOOPBACK.parse().unwrap(), None, Impairment::default())
        .await
        .unwrap();
    let peer = answering.local_addr().unwrap();
    let calling = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), Impairment::default())
        .await
        .unwrap();
    let caller_port = serve(calling, "ATE0").await;
    let isp_port = serve(answering, "ATE0S0=1").await;

    let mut isp = TcpStream::connect(isp_port).await.unwrap();
    let mut caller = TcpStream::connect(caller_port).await.unwrap();
    caller.write_all(b"ATDT0300\r").await.unwrap();
    read_until(&mut caller, b"CONNECT").await;
    read_until(&mut isp, b"CONNECT").await;

    drop(caller);
    read_until(&mut isp, b"NO CARRIER").await;

    let mut caller = TcpStream::connect(caller_port).await.unwrap();
    caller.write_all(b"AT\r").await.unwrap();
    read_until(&mut caller, b"OK\r\n").await;
}
