// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The receive path: puts RTP packets back in order and fills gaps with
//! silence. There is no playout clock, so samples leave as soon as their
//! order is known.

use std::collections::BTreeMap;

const MAX_GAP_SAMPLES: u32 = 8000;
const RESYNC_DISTANCE: i64 = 3000;

#[derive(Debug)]
pub struct Reorder {
    window: usize,
    next: Option<Position>,
    held: BTreeMap<u64, (u32, Vec<i16>)>,
}

#[derive(Debug, Clone, Copy)]
struct Position {
    sequence: u64,
    timestamp: u32,
}

impl Reorder {
    /// `window` is how many packets may wait behind a missing one before it
    /// is given up as lost.
    #[must_use]
    pub fn new(window: usize) -> Self {
        Self {
            window,
            next: None,
            held: BTreeMap::new(),
        }
    }

    /// Takes one packet and appends every frame that is now in order to `out`.
    pub fn push(
        &mut self,
        sequence: u16,
        timestamp: u32,
        samples: Vec<i16>,
        out: &mut Vec<Vec<i16>>,
    ) {
        let next = *self.next.get_or_insert(Position {
            sequence: u64::from(sequence),
            timestamp,
        });
        let distance = i64::from(sequence.wrapping_sub(truncate(next.sequence)).cast_signed());
        if distance.abs() > RESYNC_DISTANCE {
            self.flush(out);
            self.next = Some(Position {
                sequence: u64::from(sequence),
                timestamp,
            });
            self.held.insert(u64::from(sequence), (timestamp, samples));
            self.drain(out);
            return;
        }
        let Ok(offset) = u64::try_from(distance) else {
            return;
        };
        self.held
            .insert(next.sequence + offset, (timestamp, samples));
        self.drain(out);
        while self.held.len() > self.window {
            self.skip_to_first_held(out);
            self.drain(out);
        }
    }

    /// Releases everything held, filling gaps, for when the stream ends.
    pub fn flush(&mut self, out: &mut Vec<Vec<i16>>) {
        while !self.held.is_empty() {
            self.skip_to_first_held(out);
            self.drain(out);
        }
    }

    fn drain(&mut self, out: &mut Vec<Vec<i16>>) {
        let Some(next) = self.next.as_mut() else {
            return;
        };
        while let Some((timestamp, samples)) = self.held.remove(&next.sequence) {
            next.sequence += 1;
            next.timestamp = timestamp.wrapping_add(u32::try_from(samples.len()).unwrap_or(0));
            out.push(samples);
        }
    }

    fn skip_to_first_held(&mut self, out: &mut Vec<Vec<i16>>) {
        let (Some(next), Some((&sequence, &(timestamp, _)))) =
            (self.next.as_mut(), self.held.first_key_value())
        else {
            return;
        };
        let gap = timestamp.wrapping_sub(next.timestamp);
        if gap > 0 && gap <= MAX_GAP_SAMPLES {
            out.push(vec![0; gap as usize]);
        }
        next.sequence = sequence;
        next.timestamp = timestamp;
    }
}

fn truncate(sequence: u64) -> u16 {
    (sequence & 0xFFFF) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u32 = 160;

    fn frame(n: u16) -> Vec<i16> {
        vec![i16::try_from(n).unwrap() + 1; FRAME as usize]
    }

    fn run(window: usize, packets: &[u16]) -> Vec<Vec<i16>> {
        let mut reorder = Reorder::new(window);
        let mut out = Vec::new();
        for &n in packets {
            reorder.push(n, u32::from(n) * FRAME, frame(n), &mut out);
        }
        reorder.flush(&mut out);
        out
    }

    fn silence() -> Vec<i16> {
        vec![0; FRAME as usize]
    }

    #[test]
    fn passes_an_ordered_stream_straight_through() {
        assert_eq!(run(3, &[0, 1, 2]), [frame(0), frame(1), frame(2)]);
    }

    #[test]
    fn puts_swapped_packets_back_in_order() {
        assert_eq!(
            run(3, &[0, 2, 1, 3]),
            [frame(0), frame(1), frame(2), frame(3)]
        );
    }

    #[test]
    fn fills_a_lost_packet_with_silence_once_the_window_is_full() {
        let mut reorder = Reorder::new(2);
        let mut out = Vec::new();
        for n in [0, 2, 3] {
            reorder.push(n, u32::from(n) * FRAME, frame(n), &mut out);
        }
        assert_eq!(out, [frame(0)]);
        reorder.push(4, 4 * FRAME, frame(4), &mut out);
        assert_eq!(out, [frame(0), silence(), frame(2), frame(3), frame(4)]);
    }

    #[test]
    fn drops_a_packet_that_arrives_after_its_gap_was_filled() {
        assert_eq!(
            run(1, &[0, 2, 3, 1, 4]),
            [frame(0), silence(), frame(2), frame(3), frame(4)]
        );
    }

    #[test]
    fn follows_the_sequence_number_through_wraparound() {
        let mut reorder = Reorder::new(3);
        let mut out = Vec::new();
        for (i, n) in [65534u16, 65535, 0, 1].into_iter().enumerate() {
            let i = u32::try_from(i).unwrap();
            reorder.push(n, i * FRAME, silence(), &mut out);
        }
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn resynchronises_when_the_sender_jumps() {
        let out = run(3, &[0, 1, 20000, 20001]);
        assert_eq!(out, [frame(0), frame(1), frame(20000), frame(20001)]);
    }
}
