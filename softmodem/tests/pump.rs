use std::time::Duration;

use softmodem::Role;
use softmodem_transport::Transport;
use softmodem_transport::wire::{Impairment, Wire};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio::time::timeout;

const LOOPBACK: &str = "127.0.0.1:0";

fn payload() -> Vec<u8> {
    let mut rng = fastrand::Rng::with_seed(3);
    (0..60).map(|_| rng.u8(..)).collect()
}

async fn send_across(impairment: Impairment) -> Vec<u8> {
    let mut answering = Wire::bind(LOOPBACK.parse().unwrap(), None, impairment)
        .await
        .unwrap();
    let peer = answering.local_addr().unwrap();
    let mut calling = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), impairment)
        .await
        .unwrap();
    let (outgoing, incoming) = tokio::join!(calling.dial("0300"), answering.accept());

    let (mut to_originate, originate_in) = duplex(64);
    let (answer_out, mut from_answer) = duplex(4096);
    let (_keep_answer_input_open, answer_in) = duplex(64);

    to_originate.write_all(&payload()).await.unwrap();
    drop(to_originate);

    let originate = softmodem::run(
        outgoing.unwrap(),
        Role::Originate,
        originate_in,
        tokio::io::sink(),
        std::future::pending(),
    );
    let answer = softmodem::run(
        incoming.unwrap(),
        Role::Answer,
        answer_in,
        answer_out,
        std::future::pending(),
    );
    let (originated, answered) = timeout(Duration::from_secs(20), async {
        tokio::join!(originate, answer)
    })
    .await
    .expect("the call never ended");
    originated.unwrap();
    answered.unwrap();

    let mut received = Vec::new();
    from_answer.read_to_end(&mut received).await.unwrap();
    received
}

fn common_subsequence(a: &[u8], b: &[u8]) -> usize {
    let mut row = vec![0; b.len() + 1];
    for &x in a {
        let mut diagonal = 0;
        for (j, &y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if x == y {
                diagonal + 1
            } else {
                above.max(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

#[tokio::test]
async fn bytes_cross_a_clean_wire_intact() {
    assert_eq!(send_across(Impairment::default()).await, payload());
}

#[tokio::test]
async fn most_bytes_cross_a_lossy_wire() {
    let received = send_across(Impairment {
        loss: 0.02,
        reorder: 0.05,
        seed: 11,
    })
    .await;
    let kept = common_subsequence(&received, &payload());
    assert!(kept >= 45, "only {kept} of 60 bytes crossed a 2% loss wire");
}
