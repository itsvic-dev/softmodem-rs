//! The INFO sequences of § 8.2.3 that differ from those of V.34: INFO0d
//! from the digital modem, and INFO1a when the analogue modem picks V.90.
//! INFO0a has the layout of the V.34 INFO0, and INFO1d that of INFO1c.

use super::ucode::Law;
use crate::v34::SymbolRate;
use crate::v34::bits::{Reader, Writer};
use crate::v34::info::{Info, Info0, TransmitClock, narrow, read_offset, write_offset};

// Bits 37:39 of INFO1a: the digital modem sends at 8000 symbols/s.
const PCM_SYMBOL_RATE: u32 = 6;

/// Table 7: what the digital modem supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Info0d {
    /// Bits 12 to 28, as the V.34 INFO0 has them, with no transmit clock.
    pub v34: Info0,
    /// Its phase 2 power, in dB below -6 dBm0.
    pub nominal_power: u8,
    /// Its highest power, in 0.5 dB steps below -0.5 dBm0.
    pub max_power: u8,
    /// Its power is measured at the output of the codec, not at its terminals.
    pub power_at_codec: bool,
    pub law: Law,
    pub upstream_3429: bool,
}

impl Info0d {
    /// The highest power, in dBm0.
    #[must_use]
    pub fn max_power_dbm0(&self) -> f64 {
        -0.5 * f64::from(self.max_power + 1)
    }
}

impl Info for Info0d {
    const FIELDS: usize = 30;

    fn fields(&self) -> Vec<bool> {
        let mut writer = Writer::default();
        let v34 = Info0 {
            clock: TransmitClock::Internal,
            ..self.v34
        };
        writer.bits.extend(v34.fields());
        writer.field(self.nominal_power.into(), 4);
        writer.field(self.max_power.into(), 5);
        writer.flag(self.power_at_codec);
        writer.flag(self.law == Law::A);
        writer.flag(self.upstream_3429);
        writer.flag(false);
        writer.bits
    }

    fn from_fields(fields: &[bool]) -> Option<Self> {
        let (v34, rest) = fields.split_at(Info0::FIELDS);
        let mut v34 = Info0::from_fields(v34)?;
        v34.clock = TransmitClock::Internal;
        let mut reader = Reader::new(rest);
        Some(Self {
            v34,
            nominal_power: narrow(reader.field(4)),
            max_power: narrow(reader.field(5)),
            power_at_codec: reader.flag(),
            law: if reader.flag() { Law::A } else { Law::Mu },
            upstream_3429: reader.flag(),
        })
    }
}

/// Table 10: the analogue modem asks for V.90.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info1a {
    /// In 35 ms steps.
    pub md_length: u8,
    /// The Ucode of the two points that the digital modem trains with.
    pub uinfo: u8,
    /// 3000, 3200 or 3429.
    pub upstream: SymbolRate,
    /// Of the 1050 Hz probing tone, in 0.02 Hz, if it was measured.
    pub frequency_offset: Option<i16>,
}

impl Info for Info1a {
    const FIELDS: usize = 38;

    fn fields(&self) -> Vec<bool> {
        let mut writer = Writer::default();
        writer.field(0, 6);
        writer.field(self.md_length.into(), 7);
        writer.field(self.uinfo.into(), 7);
        writer.field(0, 2);
        writer.field(self.upstream.index().into(), 3);
        writer.field(PCM_SYMBOL_RATE, 3);
        write_offset(&mut writer, self.frequency_offset);
        writer.bits
    }

    fn from_fields(fields: &[bool]) -> Option<Self> {
        let mut reader = Reader::new(fields);
        reader.field(6);
        let md_length = narrow(reader.field(7));
        let uinfo = narrow(reader.field(7));
        reader.field(2);
        let upstream = SymbolRate::from_index(reader.field(3))
            .filter(|&rate| rate >= SymbolRate::S3000)?;
        (reader.field(3) == PCM_SYMBOL_RATE).then(|| Self {
            md_length,
            uinfo,
            upstream,
            frequency_offset: read_offset(&mut reader),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v34::info::{self, Deframer};

    fn info0d() -> Info0d {
        Info0d {
            v34: Info0 {
                supports_3429: true,
                high_carrier_3200: true,
                allows_3429: true,
                constellation_1664: true,
                acknowledge: true,
                ..Info0::default()
            },
            nominal_power: 7,
            max_power: 23,
            power_at_codec: true,
            law: Law::A,
            upstream_3429: true,
        }
    }

    fn info1a() -> Info1a {
        Info1a {
            md_length: 0,
            uinfo: 70,
            upstream: SymbolRate::S3200,
            frequency_offset: Some(-3),
        }
    }

    fn deframe<T: Info>(frame: Vec<bool>) -> Vec<T> {
        let mut deframer = Deframer::<T>::default();
        frame
            .into_iter()
            .filter_map(|bit| deframer.push(bit))
            .collect()
    }

    #[test]
    fn frames_have_the_lengths_of_tables_7_and_10() {
        assert_eq!(info0d().frame().len(), 62);
        assert_eq!(info1a().frame().len(), 70);
    }

    #[test]
    fn places_the_fields_of_table_7() {
        let frame = info0d().frame();
        let set: Vec<usize> = (12..42).filter(|&n| frame[n]).collect();
        assert_eq!(
            set,
            [14, 18, 19, 25, 28, 29, 30, 31, 33, 34, 35, 37, 38, 39, 40]
        );
        assert!((info0d().max_power_dbm0() + 12.0).abs() < 1e-9);
    }

    #[test]
    fn places_the_fields_of_table_10() {
        let frame = info1a().frame();
        let field = |from: usize, to: usize| {
            (from..=to)
                .rev()
                .fold(0u32, |value, n| value << 1 | u32::from(frame[n]))
        };
        assert_eq!(field(25, 31), 70);
        assert_eq!(field(34, 36), 4);
        assert_eq!(field(37, 39), 6);
    }

    #[test]
    fn reads_back_what_it_frames() {
        assert_eq!(deframe::<Info0d>(info0d().frame()), [info0d()]);
        assert_eq!(deframe::<Info1a>(info1a().frame()), [info1a()]);
    }

    #[test]
    fn tells_the_two_forms_of_info1a_apart() {
        assert!(deframe::<info::Info1a>(info1a().frame()).is_empty());
        let v34 = info::Info1a {
            min_power_reduction: 0,
            extra_power_reduction: 0,
            md_length: 0,
            probe: info::Probe::default(),
            answer_to_call: SymbolRate::S3200,
            call_to_answer: SymbolRate::S3200,
            frequency_offset: None,
        };
        assert!(deframe::<Info1a>(v34.frame()).is_empty());
    }
}
