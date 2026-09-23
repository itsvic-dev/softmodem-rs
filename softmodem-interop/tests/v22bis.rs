mod common;

use common::{FRAMES, exchange, payload, reversed};
use softmodem_interop::{GuardTone, V22bis};

#[test]
fn spandsp_trains_at_2400_and_carries_data_with_itself() {
    let mut caller = V22bis::new(2400, true, GuardTone::Hz1800);
    let mut answerer = V22bis::new(2400, false, GuardTone::Hz1800);

    exchange(&mut caller, &mut answerer, FRAMES);
    assert!(caller.trained() && answerer.trained());
    assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));

    caller.send(&payload());
    answerer.send(&reversed());
    exchange(&mut caller, &mut answerer, FRAMES);
    assert_eq!(answerer.bytes(), payload());
    assert_eq!(caller.bytes(), reversed());
}
