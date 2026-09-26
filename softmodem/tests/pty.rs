// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use softmodem::{Modem, profile};
use softmodem_terminal::pty::Pty;
use softmodem_transport::wire::{Impairment, Wire};
use tokio::time::timeout;

const LOOPBACK: &str = "127.0.0.1:0";

fn every_byte() -> Vec<u8> {
    (0..=255).collect()
}

fn open(path: PathBuf) -> File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

fn read_until(port: &mut File, marker: &[u8]) {
    let mut seen = Vec::new();
    let mut byte = [0];
    while !seen.ends_with(marker) {
        port.read_exact(&mut byte).unwrap();
        seen.push(byte[0]);
    }
}

fn serve(wire: Wire, init: &str) -> PathBuf {
    let pty = Pty::open(None).unwrap();
    let path = pty.path().to_owned();
    let profile = profile(init).unwrap();
    tokio::spawn(async move {
        Modem::new(wire, pty, profile, |call, _| (call, None))
            .run(std::future::pending())
            .await
            .unwrap();
    });
    path
}

#[tokio::test]
async fn a_computer_dials_and_every_byte_reaches_the_other_computer() {
    let answering = Wire::bind(LOOPBACK.parse().unwrap(), None, Impairment::default())
        .await
        .unwrap();
    let peer = answering.local_addr().unwrap();
    let calling = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), Impairment::default())
        .await
        .unwrap();
    let caller_port = serve(calling, "ATE0");
    let isp_port = serve(answering, "ATE0S0=1");

    let computers = tokio::task::spawn_blocking(move || {
        let mut isp = open(isp_port);
        let mut caller = open(caller_port);
        caller.write_all(b"ATDT0300\r").unwrap();
        for port in [&mut caller, &mut isp] {
            read_until(port, b"CONNECT");
            read_until(port, b"\r\n");
        }
        caller.write_all(&every_byte()).unwrap();
        let mut received = vec![0; 256];
        isp.read_exact(&mut received).unwrap();
        received
    });
    let received = timeout(Duration::from_secs(40), computers)
        .await
        .expect("the bytes never arrived")
        .unwrap();
    assert_eq!(received, every_byte());
}
