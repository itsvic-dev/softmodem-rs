use std::net::SocketAddr;
use std::time::Duration;

use softmodem_transport::wire::{Impairment, Wire};
use softmodem_transport::{Call, DialError, FRAME_SAMPLES, Incoming, Transport, alaw, wav};
use tokio::time::timeout;

const LOOPBACK: &str = "127.0.0.1:0";

async fn line(impairment: Impairment) -> (Wire, Wire) {
    let answering = Wire::bind(LOOPBACK.parse().unwrap(), None, Impairment::default())
        .await
        .unwrap();
    let peer: SocketAddr = answering.local_addr().unwrap();
    let calling = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), impairment)
        .await
        .unwrap();
    (calling, answering)
}

async fn ringing(answering: &mut Wire) -> SocketAddr {
    match answering.incoming().await.unwrap() {
        Incoming::Ringing { caller, number } => {
            assert_eq!(number, "0300");
            caller
        }
        Incoming::Gone(caller) => panic!("{caller} gave up before ringing"),
    }
}

async fn connect(impairment: Impairment) -> (Call, Call) {
    let (mut calling, mut answering) = line(impairment).await;
    let answer = async {
        let caller = ringing(&mut answering).await;
        answering.answer(&caller).await
    };
    let (outgoing, incoming) = tokio::join!(calling.dial("0300"), answer);
    (outgoing.unwrap(), incoming.unwrap())
}

#[tokio::test]
async fn a_rejected_call_is_busy() {
    let (mut calling, mut answering) = line(Impairment::default()).await;
    let reject = async {
        let caller = ringing(&mut answering).await;
        answering.reject(&caller).await.unwrap();
    };
    let (dialled, ()) = tokio::join!(calling.dial("0300"), reject);
    assert!(matches!(dialled, Err(DialError::Busy)));
}

#[tokio::test]
async fn a_second_caller_is_busy_while_the_first_rings() {
    let (mut first, mut answering) = line(Impairment::default()).await;
    let peer = answering.local_addr().unwrap();
    let mut second = Wire::bind(LOOPBACK.parse().unwrap(), Some(peer), Impairment::default())
        .await
        .unwrap();
    let first_dial = tokio::spawn(async move { first.dial("0300").await });
    ringing(&mut answering).await;
    let (second_dial, still_ringing) = tokio::join!(
        second.dial("0300"),
        timeout(Duration::from_secs(1), answering.incoming())
    );
    assert!(matches!(second_dial, Err(DialError::Busy)));
    assert!(still_ringing.is_err(), "the first call stopped ringing");
    first_dial.abort();
}

#[tokio::test]
async fn an_abandoned_call_stops_ringing() {
    let (mut calling, mut answering) = line(Impairment::default()).await;
    let dialling = tokio::spawn(async move { calling.dial("0300").await });
    let caller = ringing(&mut answering).await;
    dialling.abort();
    let next = timeout(Duration::from_secs(1), answering.incoming()).await;
    assert_eq!(next.unwrap().unwrap(), Incoming::Gone(caller));
}

fn frame(n: usize) -> Vec<i16> {
    let level = alaw::decode(alaw::encode(i16::try_from(n * 100).unwrap()));
    vec![level; FRAME_SAMPLES]
}

async fn receive_all(call: &mut Call) -> Vec<Vec<i16>> {
    let mut frames = Vec::new();
    while let Ok(Some(frame)) = timeout(Duration::from_secs(2), call.audio_in.recv()).await {
        frames.push(frame);
    }
    frames
}

fn read_wav(path: &std::path::Path) -> Vec<i16> {
    hound::WavReader::open(path)
        .unwrap()
        .into_samples()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test]
async fn carries_frames_in_order_and_ends_on_hang_up() {
    let (outgoing, mut incoming) = connect(Impairment::default()).await;
    for n in 0..10 {
        outgoing.audio_out.send(frame(n)).await.unwrap();
    }
    drop(outgoing);
    let expected: Vec<_> = (0..10).map(frame).collect();
    assert_eq!(receive_all(&mut incoming).await, expected);
    assert!(incoming.audio_in.is_closed());
}

#[tokio::test]
async fn fills_lost_packets_with_silence_and_undoes_reordering() {
    let impairment = Impairment {
        loss: 0.1,
        reorder: 0.2,
        seed: 7,
    };
    let (outgoing, mut incoming) = connect(impairment).await;
    for n in 0..100 {
        outgoing.audio_out.send(frame(n)).await.unwrap();
    }
    drop(outgoing);
    let received = receive_all(&mut incoming).await;

    let samples: usize = received.iter().map(Vec::len).sum();
    assert!(samples <= 100 * FRAME_SAMPLES);
    let levels: Vec<i16> = received
        .iter()
        .filter(|f| f[0] != 0)
        .map(|f| f[0])
        .collect();
    assert!(
        levels.is_sorted(),
        "frames arrived out of order: {levels:?}"
    );
    assert!(
        levels.len() > 70,
        "far more loss than configured: {}",
        levels.len()
    );
}

#[tokio::test]
async fn records_both_directions() {
    let directory = std::env::temp_dir().join(format!("softmodem-wav-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let prefix = directory.join("call");

    let (outgoing, incoming) = connect(Impairment::default()).await;
    let mut outgoing = wav::record(outgoing, &prefix).unwrap();
    for n in 0..5 {
        outgoing.audio_out.send(frame(n)).await.unwrap();
        incoming.audio_out.send(frame(n + 5)).await.unwrap();
    }
    drop(incoming);
    let heard = receive_all(&mut outgoing).await;
    drop(outgoing);
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(
        read_wav(&directory.join("call-tx.wav")),
        (0..5).flat_map(frame).collect::<Vec<_>>()
    );
    assert_eq!(read_wav(&directory.join("call-rx.wav")), heard.concat());
    std::fs::remove_dir_all(directory).unwrap();
}
