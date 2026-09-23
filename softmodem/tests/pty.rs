use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use softmodem::Role;
use softmodem_terminal::pty::Pty;
use softmodem_transport::Transport;
use softmodem_transport::wire::{Impairment, Wire};
use tokio::time::timeout;

const LOOPBACK: &str = "127.0.0.1:0";

fn every_byte() -> Vec<u8> {
    (0..=255).collect()
}

fn open(path: PathBuf) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap()
}

#[tokio::test]
async fn every_byte_crosses_from_one_serial_port_to_the_other() {
    let mut answering = Wire::bind(LOOPBACK.parse().unwrap(), None, Impairment::default())
        .await
        .unwrap();
    let peer = answering.local_addr().unwrap();
    let mut calling = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), Impairment::default())
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::join!(calling.dial("0300"), answering.accept());

    let originate_port = Pty::open(None).unwrap();
    let answer_port = Pty::open(None).unwrap();
    let sending_to = originate_port.path().to_owned();
    let receiving_from = answer_port.path().to_owned();

    let programs = tokio::task::spawn_blocking(move || {
        let mut answer_side = open(receiving_from);
        open(sending_to).write_all(&every_byte()).unwrap();
        let mut bytes = vec![0; 256];
        answer_side.read_exact(&mut bytes).unwrap();
        bytes
    });
    let modems = async {
        tokio::join!(
            softmodem::run(
                outgoing.unwrap(),
                Role::Originate,
                &originate_port,
                &originate_port,
                std::future::pending(),
            ),
            softmodem::run(
                incoming.unwrap(),
                Role::Answer,
                &answer_port,
                &answer_port,
                std::future::pending(),
            ),
        )
    };

    tokio::select! {
        _ = modems => panic!("the call ended before the bytes arrived"),
        received = timeout(Duration::from_secs(20), programs) => {
            let received = received.expect("the bytes never arrived").unwrap();
            assert_eq!(received, every_byte());
        }
    }
}
