//! 8N1 asynchronous framing: a start bit, eight data bits LSB first, a stop
//! bit. The idle line is mark, which is `true`.

pub fn frame(byte: u8) -> impl Iterator<Item = bool> {
    std::iter::once(false)
        .chain((0..8).map(move |i| byte >> i & 1 == 1))
        .chain(std::iter::once(true))
}

#[derive(Debug, Default)]
pub struct Decoder {
    state: State,
    framing_errors: u64,
}

#[derive(Debug, Default, Clone, Copy)]
enum State {
    #[default]
    Idle,
    Data {
        byte: u8,
        count: u8,
    },
    Stop {
        byte: u8,
    },
    // Waits for mark, so one bad stop bit does not misframe what follows.
    Hunt,
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bit: bool) -> Option<u8> {
        let (next, out) = match (self.state, bit) {
            (State::Idle, false) => (State::Data { byte: 0, count: 0 }, None),
            (State::Idle | State::Hunt, true) => (State::Idle, None),
            (State::Hunt, false) => (State::Hunt, None),
            (State::Data { byte, count }, bit) => {
                let byte = byte | u8::from(bit) << count;
                if count == 7 {
                    (State::Stop { byte }, None)
                } else {
                    (
                        State::Data {
                            byte,
                            count: count + 1,
                        },
                        None,
                    )
                }
            }
            (State::Stop { byte }, true) => (State::Idle, Some(byte)),
            (State::Stop { .. }, false) => {
                self.framing_errors += 1;
                (State::Hunt, None)
            }
        };
        self.state = next;
        out
    }

    /// Drops a partly received character, for when carrier is lost.
    pub fn reset(&mut self) {
        self.state = State::Idle;
    }

    #[must_use]
    pub fn framing_errors(&self) -> u64 {
        self.framing_errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bits: impl IntoIterator<Item = bool>) -> (Vec<u8>, u64) {
        let mut decoder = Decoder::new();
        let bytes = bits.into_iter().filter_map(|b| decoder.push(b)).collect();
        (bytes, decoder.framing_errors())
    }

    #[test]
    fn frames_lsb_first_between_start_and_stop() {
        let bits: Vec<bool> = frame(0b1000_0001).collect();
        assert_eq!(
            bits,
            [
                false, true, false, false, false, false, false, false, true, true
            ]
        );
    }

    #[test]
    fn round_trips_every_byte() {
        let input: Vec<u8> = (0..=255).collect();
        let (output, errors) = decode(input.iter().flat_map(|&b| frame(b)));
        assert_eq!(output, input);
        assert_eq!(errors, 0);
    }

    #[test]
    fn idle_mark_yields_nothing() {
        assert_eq!(decode(std::iter::repeat_n(true, 100)), (vec![], 0));
    }

    #[test]
    fn resynchronises_after_a_framing_error() {
        let mut bits: Vec<bool> = frame(b'A').collect();
        *bits.last_mut().unwrap() = false;
        bits.extend(std::iter::repeat_n(true, 3));
        bits.extend(frame(b'B'));
        assert_eq!(decode(bits), (vec![b'B'], 1));
    }
}
