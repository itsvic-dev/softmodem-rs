use softmodem_interop::{GuardTone, V22};

const FRAME: usize = 160;

fn payload() -> Vec<u8> {
    (0..=255).collect()
}

fn exchange(caller: &mut V22, answerer: &mut V22, frames: usize) {
    let mut up = [0; FRAME];
    let mut down = [0; FRAME];
    for _ in 0..frames {
        caller.render(&mut up);
        answerer.render(&mut down);
        answerer.process(&up);
        caller.process(&down);
    }
}

#[test]
fn spandsp_trains_and_carries_data_with_itself() {
    let mut caller = V22::new(true, GuardTone::Hz1800);
    let mut answerer = V22::new(false, GuardTone::Hz1800);

    exchange(&mut caller, &mut answerer, 150);
    assert!(caller.trained() && answerer.trained());
    assert_eq!((caller.bit_rate(), answerer.bit_rate()), (1200, 1200));

    caller.send(&payload());
    answerer.send(&payload().into_iter().rev().collect::<Vec<_>>());
    exchange(&mut caller, &mut answerer, 150);
    assert_eq!(answerer.bytes(), payload());
    assert_eq!(
        caller.bytes(),
        payload().into_iter().rev().collect::<Vec<_>>()
    );
}
