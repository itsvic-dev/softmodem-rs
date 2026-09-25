// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{Read, Write};
use std::time::Duration;

use softmodem_terminal::port::SerialPort;
use softmodem_terminal::pty::Pty;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

fn every_byte() -> Vec<u8> {
    (0..=255).collect()
}

#[tokio::test]
async fn is_8_bit_clean_in_both_directions() {
    let pty = Pty::open(None).unwrap();
    let path = pty.path().to_owned();

    let program = tokio::task::spawn_blocking(move || {
        let mut port = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        port.write_all(&every_byte()).unwrap();
        let mut echoed = vec![0; 256];
        port.read_exact(&mut echoed).unwrap();
        echoed
    });

    let mut master = &pty;
    let mut received = vec![0; 256];
    timeout(Duration::from_secs(5), master.read_exact(&mut received))
        .await
        .expect("the program's bytes never reached the master")
        .unwrap();
    assert_eq!(received, every_byte());

    master.write_all(&every_byte()).await.unwrap();
    let echoed = timeout(Duration::from_secs(5), program)
        .await
        .expect("the master's bytes never reached the program")
        .unwrap();
    assert_eq!(echoed, every_byte());
}

#[tokio::test]
async fn dropping_carrier_hangs_up_the_program_and_moves_the_link() {
    let link = std::env::temp_dir().join(format!("softmodem-hangup-{}", std::process::id()));
    let mut pty = Pty::open(Some(&link)).unwrap();
    let first = pty.path().to_owned();
    let mut program = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&link)
        .unwrap();

    pty.set_carrier(true).unwrap();
    pty.set_carrier(false).unwrap();

    let mut buf = [0; 1];
    let hung_up = match program.read(&mut buf) {
        Ok(0) => true,
        Err(error) => error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()),
        Ok(_) => false,
    };
    assert!(hung_up, "the program did not see a hang-up");
    assert_ne!(std::fs::read_link(&link).unwrap(), first);

    let mut reopened = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&link)
        .unwrap();
    reopened.write_all(b"x").unwrap();
    let mut master = &pty;
    let mut received = [0; 1];
    timeout(Duration::from_secs(5), master.read_exact(&mut received))
        .await
        .expect("the new port does not reach the master")
        .unwrap();
    assert_eq!(&received, b"x");
}

#[tokio::test]
async fn keeps_the_port_up_while_carrier_stays_on() {
    let mut pty = Pty::open(None).unwrap();
    let path = pty.path().to_owned();
    pty.set_carrier(true).unwrap();
    pty.set_carrier(true).unwrap();
    assert_eq!(pty.path(), path);
}

#[tokio::test]
async fn links_to_the_slave_and_removes_the_link_on_drop() {
    let link = std::env::temp_dir().join(format!("softmodem-pty-{}", std::process::id()));
    let pty = Pty::open(Some(&link)).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), pty.path());
    drop(pty);
    assert!(!link.exists());
}
