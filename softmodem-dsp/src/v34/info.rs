//! The INFO sequences of § 10.1.2.3, which carry capabilities and the
//! results of line probing in phase 2.

use std::collections::VecDeque;
use std::marker::PhantomData;

use super::SymbolRate;
use super::bits::{self, Reader, Writer};

const FILL: &str = "1111";
const SYNC: &str = "01110010";
const HEADER: [bool; 12] = [
    true, true, true, true, false, true, true, true, false, false, true, false,
];
const CRC: usize = 16;
// Bits 79:88 of INFO1c and 40:49 of INFO1a: this value means "ignore".
const NO_OFFSET: i16 = -512;

/// An INFO sequence, as the fields between its frame sync and its CRC.
pub trait Info: Sized {
    /// How many bits the fields take.
    const FIELDS: usize;

    fn fields(&self) -> Vec<bool>;

    fn from_fields(fields: &[bool]) -> Option<Self>;

    /// Fill, frame sync, fields, CRC and fill, bit 0 first.
    fn frame(&self) -> Vec<bool> {
        let fields = self.fields();
        let mut writer = Writer::default();
        writer.pattern(FILL);
        writer.pattern(SYNC);
        writer.bits.extend(&fields);
        writer.field(u32::from(bits::crc(fields)), 16);
        writer.pattern(FILL);
        writer.bits
    }
}

/// Finds INFO sequences of one kind in received bits, and keeps those whose
/// CRC holds.
#[derive(Debug)]
pub struct Deframer<T> {
    window: VecDeque<bool>,
    kind: PhantomData<T>,
}

impl<T> Default for Deframer<T> {
    fn default() -> Self {
        Self {
            window: VecDeque::new(),
            kind: PhantomData,
        }
    }
}

impl<T: Info> Deframer<T> {
    pub fn push(&mut self, bit: bool) -> Option<T> {
        let length = HEADER.len() + T::FIELDS + CRC;
        self.window.push_back(bit);
        if self.window.len() > length {
            self.window.pop_front();
        }
        if self.window.len() < length
            || !self.window.iter().take(HEADER.len()).eq(&HEADER)
            || bits::crc(self.window.iter().skip(HEADER.len()).copied()) != 0
        {
            return None;
        }
        let fields: Vec<bool> = self
            .window
            .iter()
            .skip(HEADER.len())
            .take(T::FIELDS)
            .copied()
            .collect();
        self.window.clear();
        T::from_fields(&fields)
    }
}

/// Bits 26:27 of INFO0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TransmitClock {
    #[default]
    Internal,
    FromReceiver,
    External,
}

/// Table 14: what a modem supports, sent by each end first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per flag of table 14"
)]
pub struct Info0 {
    pub supports_2743: bool,
    pub supports_2800: bool,
    pub supports_3429: bool,
    /// The carriers its transmitter may use at 3000 and 3200.
    pub low_carrier_3000: bool,
    pub high_carrier_3000: bool,
    pub low_carrier_3200: bool,
    pub high_carrier_3200: bool,
    /// False where regulations disallow 3429.
    pub allows_3429: bool,
    pub reduces_power: bool,
    /// How many symbol rate steps the two directions may differ by.
    pub rate_difference: u8,
    pub cme: bool,
    pub constellation_1664: bool,
    pub clock: TransmitClock,
    /// Set once the far INFO0 has arrived.
    pub acknowledge: bool,
}

impl Info for Info0 {
    const FIELDS: usize = 17;

    fn fields(&self) -> Vec<bool> {
        let mut writer = Writer::default();
        for flag in [
            self.supports_2743,
            self.supports_2800,
            self.supports_3429,
            self.low_carrier_3000,
            self.high_carrier_3000,
            self.low_carrier_3200,
            self.high_carrier_3200,
            self.allows_3429,
            self.reduces_power,
        ] {
            writer.flag(flag);
        }
        writer.field(self.rate_difference.into(), 3);
        writer.flag(self.cme);
        writer.flag(self.constellation_1664);
        writer.field(
            match self.clock {
                TransmitClock::Internal => 0,
                TransmitClock::FromReceiver => 1,
                TransmitClock::External => 2,
            },
            2,
        );
        writer.flag(self.acknowledge);
        writer.bits
    }

    fn from_fields(fields: &[bool]) -> Option<Self> {
        let mut reader = Reader::new(fields);
        let mut flags = [false; 9];
        for flag in &mut flags {
            *flag = reader.flag();
        }
        let rate_difference = narrow(reader.field(3));
        let cme = reader.flag();
        let constellation_1664 = reader.flag();
        let clock = match reader.field(2) {
            0 => TransmitClock::Internal,
            1 => TransmitClock::FromReceiver,
            2 => TransmitClock::External,
            _ => return None,
        };
        Some(Self {
            supports_2743: flags[0],
            supports_2800: flags[1],
            supports_3429: flags[2],
            low_carrier_3000: flags[3],
            high_carrier_3000: flags[4],
            low_carrier_3200: flags[5],
            high_carrier_3200: flags[6],
            allows_3429: flags[7],
            reduces_power: flags[8],
            rate_difference,
            cme,
            constellation_1664,
            clock,
            acknowledge: reader.flag(),
        })
    }
}

/// What line probing found for one symbol rate, for the transmitter at the
/// far end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Probe {
    pub high_carrier: bool,
    /// The index of tables 3 and 4, 0 to 10.
    pub pre_emphasis: u8,
    /// In multiples of 2400 bit/s, 0 if the symbol rate cannot be used.
    pub max_rate: u8,
}

impl Probe {
    fn write(self, writer: &mut Writer) {
        writer.flag(self.high_carrier);
        writer.field(self.pre_emphasis.into(), 4);
        writer.field(self.max_rate.into(), 4);
    }

    fn read(reader: &mut Reader) -> Self {
        Self {
            high_carrier: reader.flag(),
            pre_emphasis: narrow(reader.field(4)),
            max_rate: narrow(reader.field(4)),
        }
    }
}

/// Table 15: the call modem's probing results, sent after L1 and L2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Info1c {
    /// In dB, for the answer modem's transmitter.
    pub min_power_reduction: u8,
    pub extra_power_reduction: u8,
    /// In 35 ms steps.
    pub md_length: u8,
    /// For each symbol rate, in the order of `SymbolRate::ALL`.
    pub probes: [Probe; 6],
    /// Of the 1050 Hz probing tone, in 0.02 Hz, if it was measured.
    pub frequency_offset: Option<i16>,
}

impl Info for Info1c {
    const FIELDS: usize = 77;

    fn fields(&self) -> Vec<bool> {
        let mut writer = Writer::default();
        write_power_and_md(
            &mut writer,
            self.min_power_reduction,
            self.extra_power_reduction,
            self.md_length,
        );
        for probe in self.probes {
            probe.write(&mut writer);
        }
        write_offset(&mut writer, self.frequency_offset);
        writer.bits
    }

    fn from_fields(fields: &[bool]) -> Option<Self> {
        let mut reader = Reader::new(fields);
        let (min_power_reduction, extra_power_reduction, md_length) =
            read_power_and_md(&mut reader);
        let mut probes = [Probe::default(); 6];
        for probe in &mut probes {
            *probe = Probe::read(&mut reader);
        }
        Some(Self {
            min_power_reduction,
            extra_power_reduction,
            md_length,
            probes,
            frequency_offset: read_offset(&mut reader),
        })
    }
}

/// Table 16: the answer modem's probing results, and the symbol rate of
/// each direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info1a {
    /// In dB, for the call modem's transmitter.
    pub min_power_reduction: u8,
    pub extra_power_reduction: u8,
    /// In 35 ms steps.
    pub md_length: u8,
    /// For the call modem's transmitter.
    pub probe: Probe,
    pub answer_to_call: SymbolRate,
    pub call_to_answer: SymbolRate,
    /// Of the 1050 Hz probing tone, in 0.02 Hz, if it was measured.
    pub frequency_offset: Option<i16>,
}

impl Info for Info1a {
    const FIELDS: usize = 38;

    fn fields(&self) -> Vec<bool> {
        let mut writer = Writer::default();
        write_power_and_md(
            &mut writer,
            self.min_power_reduction,
            self.extra_power_reduction,
            self.md_length,
        );
        self.probe.write(&mut writer);
        writer.field(self.answer_to_call.index().into(), 3);
        writer.field(self.call_to_answer.index().into(), 3);
        write_offset(&mut writer, self.frequency_offset);
        writer.bits
    }

    fn from_fields(fields: &[bool]) -> Option<Self> {
        let mut reader = Reader::new(fields);
        let (min_power_reduction, extra_power_reduction, md_length) =
            read_power_and_md(&mut reader);
        let probe = Probe::read(&mut reader);
        let answer_to_call = SymbolRate::from_index(reader.field(3))?;
        let call_to_answer = SymbolRate::from_index(reader.field(3))?;
        Some(Self {
            min_power_reduction,
            extra_power_reduction,
            md_length,
            probe,
            answer_to_call,
            call_to_answer,
            frequency_offset: read_offset(&mut reader),
        })
    }
}

fn write_power_and_md(writer: &mut Writer, min: u8, extra: u8, md_length: u8) {
    writer.field(min.into(), 3);
    writer.field(extra.into(), 3);
    writer.field(md_length.into(), 7);
}

fn read_power_and_md(reader: &mut Reader) -> (u8, u8, u8) {
    (
        narrow(reader.field(3)),
        narrow(reader.field(3)),
        narrow(reader.field(7)),
    )
}

fn write_offset(writer: &mut Writer, offset: Option<i16>) {
    let offset = offset.map_or(NO_OFFSET, |offset| offset.clamp(-511, 511));
    writer.field(u32::from(offset.cast_unsigned() & 0x3FF), 10);
}

fn read_offset(reader: &mut Reader) -> Option<i16> {
    let raw = narrow::<u16>(reader.field(10));
    let offset = (raw << 6).cast_signed() >> 6;
    (offset != NO_OFFSET).then_some(offset)
}

fn narrow<T: TryFrom<u32>>(value: u32) -> T {
    T::try_from(value).ok().expect("a field fits its width")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info0() -> Info0 {
        Info0 {
            supports_2743: true,
            supports_3429: true,
            high_carrier_3000: true,
            low_carrier_3200: true,
            allows_3429: true,
            rate_difference: 2,
            constellation_1664: true,
            clock: TransmitClock::FromReceiver,
            acknowledge: true,
            ..Info0::default()
        }
    }

    fn info1c() -> Info1c {
        let mut probes = [Probe::default(); 6];
        for (n, probe) in (0u8..).zip(&mut probes) {
            *probe = Probe {
                high_carrier: n % 2 == 1,
                pre_emphasis: n + 3,
                max_rate: n * 2 + 1,
            };
        }
        Info1c {
            min_power_reduction: 3,
            extra_power_reduction: 5,
            md_length: 100,
            probes,
            frequency_offset: Some(-37),
        }
    }

    fn info1a() -> Info1a {
        Info1a {
            min_power_reduction: 1,
            extra_power_reduction: 7,
            md_length: 0,
            probe: Probe {
                high_carrier: true,
                pre_emphasis: 10,
                max_rate: 14,
            },
            answer_to_call: SymbolRate::S3200,
            call_to_answer: SymbolRate::S3000,
            frequency_offset: None,
        }
    }

    fn deframe<T: Info>(bits: impl IntoIterator<Item = bool>) -> Vec<T> {
        let mut deframer = Deframer::<T>::default();
        bits.into_iter()
            .filter_map(|bit| deframer.push(bit))
            .collect()
    }

    fn noise() -> impl Iterator<Item = bool> {
        (0u32..300).map(|n| n.wrapping_mul(2_654_435_761) >> 31 == 1)
    }

    #[test]
    fn frames_have_the_lengths_of_tables_14_to_16() {
        assert_eq!(info0().frame().len(), 49);
        assert_eq!(info1c().frame().len(), 109);
        assert_eq!(info1a().frame().len(), 70);
    }

    #[test]
    fn puts_the_frame_sync_at_bits_4_to_11() {
        assert_eq!(info0().frame()[4..12], bits::pattern(SYNC));
        assert_eq!(info0().frame()[45..], bits::pattern(FILL));
    }

    #[test]
    fn places_info0_fields_at_their_bits() {
        let frame = info0().frame();
        let set: Vec<usize> = (12..29).filter(|&n| frame[n]).collect();
        assert_eq!(set, [12, 14, 16, 17, 19, 22, 25, 26, 28]);
    }

    #[test]
    fn places_the_sign_of_the_offset_at_bit_88_of_info1c() {
        assert!(info1c().frame()[88]);
        let offset = Info1c {
            frequency_offset: Some(37),
            ..info1c()
        };
        assert!(!offset.frame()[88]);
    }

    #[test]
    fn finds_each_kind_among_other_bits() {
        let bits = |frame: Vec<bool>| noise().chain(frame).chain(noise());
        assert_eq!(deframe::<Info0>(bits(info0().frame())), [info0()]);
        assert_eq!(deframe::<Info1c>(bits(info1c().frame())), [info1c()]);
        assert_eq!(deframe::<Info1a>(bits(info1a().frame())), [info1a()]);
    }

    #[test]
    fn finds_repeated_sequences() {
        let bits: Vec<bool> = (0..3).flat_map(|_| info0().frame()).collect();
        assert_eq!(deframe::<Info0>(bits).len(), 3);
    }

    #[test]
    fn drops_a_sequence_with_a_flipped_bit() {
        for n in 12..45 {
            let mut frame = info0().frame();
            frame[n] = !frame[n];
            assert!(deframe::<Info0>(frame).is_empty(), "bit {n}");
        }
    }

    #[test]
    fn marks_an_offset_it_could_not_measure() {
        assert_eq!(info1a().frame()[40..50], bits::pattern("0000000001"));
    }

    #[test]
    fn keeps_offsets_within_their_ten_bits() {
        let far = Info1c {
            frequency_offset: Some(-2000),
            ..info1c()
        };
        let found = deframe::<Info1c>(far.frame());
        assert_eq!(found[0].frequency_offset, Some(-511));
    }
}
