use std::time::Duration;

use softmodem_terminal::tcp::TcpPort;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

const PATIENCE: Duration = Duration::from_secs(5);

fn every_byte() -> Vec<u8> {
    (0..=255).collect()
}

async fn listen() -> TcpPort {
    TcpPort::bind("127.0.0.1:0".parse().unwrap()).await.unwrap()
}

#[tokio::test]
async fn is_8_bit_clean_in_both_directions() {
    let mut port = listen().await;
    let mut computer = TcpStream::connect(port.local_addr().unwrap())
        .await
        .unwrap();

    computer.write_all(&every_byte()).await.unwrap();
    let mut received = vec![0; 256];
    timeout(PATIENCE, port.read_exact(&mut received))
        .await
        .expect("the computer's bytes never reached the port")
        .unwrap();
    assert_eq!(received, every_byte());

    port.write_all(&every_byte()).await.unwrap();
    let mut echoed = vec![0; 256];
    timeout(PATIENCE, computer.read_exact(&mut echoed))
        .await
        .expect("the port's bytes never reached the computer")
        .unwrap();
    assert_eq!(echoed, every_byte());
}

#[tokio::test]
async fn ends_input_once_when_the_computer_leaves_and_takes_the_next() {
    let mut port = listen().await;
    let address = port.local_addr().unwrap();
    let mut first = TcpStream::connect(address).await.unwrap();
    first.write_all(b"a").await.unwrap();
    let mut buf = [0; 16];
    assert_eq!(port.read(&mut buf).await.unwrap(), 1);

    drop(first);
    let n = timeout(PATIENCE, port.read(&mut buf))
        .await
        .expect("the port never saw the computer leave")
        .unwrap();
    assert_eq!(n, 0);

    let mut second = TcpStream::connect(address).await.unwrap();
    second.write_all(b"b").await.unwrap();
    let n = timeout(PATIENCE, port.read(&mut buf))
        .await
        .expect("the port never took the next computer")
        .unwrap();
    assert_eq!(&buf[..n], b"b");
}

#[tokio::test]
async fn loses_writes_while_no_computer_is_connected() {
    let mut port = listen().await;
    timeout(PATIENCE, port.write_all(b"RING\r\n"))
        .await
        .expect("a write waited for a computer")
        .unwrap();
}
