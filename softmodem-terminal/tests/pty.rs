use std::io::{Read, Write};
use std::time::Duration;

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
async fn links_to_the_slave_and_removes_the_link_on_drop() {
    let link = std::env::temp_dir().join(format!("softmodem-pty-{}", std::process::id()));
    let pty = Pty::open(Some(&link)).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), pty.path());
    drop(pty);
    assert!(!link.exists());
}
