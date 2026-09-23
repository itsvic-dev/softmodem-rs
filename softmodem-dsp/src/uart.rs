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
    v14: bool,
    deleted_stop: bool,
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

    /// A decoder for start-stop characters carried over a synchronous channel
    /// as V.14 describes. It also accepts a character whose stop bit a fast
    /// sender deleted, though not two such characters in a row.
    #[must_use]
    pub fn v14() -> Self {
        Self {
            v14: true,
            ..Self::default()
        }
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
            (State::Stop { byte }, true) => {
                self.deleted_stop = false;
                (State::Idle, Some(byte))
            }
            (State::Stop { byte }, false) if self.v14 && !self.deleted_stop => {
                self.deleted_stop = true;
                (State::Data { byte: 0, count: 0 }, Some(byte))
            }
            (State::Stop { .. }, false) => {
                self.deleted_stop = false;
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
        self.deleted_stop = false;
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
        decode_with(Decoder::new(), bits)
    }

    fn decode_with(mut decoder: Decoder, bits: impl IntoIterator<Item = bool>) -> (Vec<u8>, u64) {
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

    fn without_stop_bit(byte: u8) -> impl Iterator<Item = bool> {
        frame(byte).take(9)
    }

    #[test]
    fn v14_accepts_a_deleted_stop_bit() {
        let bits: Vec<bool> = without_stop_bit(b'A')
            .chain(frame(b'B'))
            .chain(without_stop_bit(b'C'))
            .chain(frame(b'D'))
            .collect();
        assert_eq!(
            decode_with(Decoder::v14(), bits.clone()),
            (b"ABCD".to_vec(), 0)
        );
        assert_ne!(decode(bits).0, b"ABCD");
    }

    #[test]
    fn v14_still_rejects_two_deleted_stop_bits_in_a_row() {
        let bits: Vec<bool> = without_stop_bit(b'A')
            .chain(without_stop_bit(b'B'))
            .chain([false])
            .chain(std::iter::repeat_n(true, 3))
            .chain(frame(b'D'))
            .collect();
        assert_eq!(decode_with(Decoder::v14(), bits), (b"AD".to_vec(), 1));
    }

    #[test]
    fn v14_counts_a_break_as_a_framing_error() {
        let bits = std::iter::repeat_n(false, 23).chain(std::iter::repeat_n(true, 20));
        let (bytes, errors) = decode_with(Decoder::v14(), bits);
        assert_eq!(bytes, [0]);
        assert_eq!(errors, 1);
    }
}
