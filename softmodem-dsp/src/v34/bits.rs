//! Fields that go least significant bit first, and the CRC of § 10.1.2.3.2.

/// x¹⁶ + x¹² + x⁵ + 1, shifted towards bit 0 as figure 14 draws it.
const CRC_POLYNOMIAL: u16 = 0x8408;

pub fn crc(bits: impl IntoIterator<Item = bool>) -> u16 {
    bits.into_iter().fold(0xFFFF, |crc, bit| {
        let feedback = (crc & 1 == 1) ^ bit;
        let crc = crc >> 1;
        if feedback { crc ^ CRC_POLYNOMIAL } else { crc }
    })
}

#[derive(Debug, Default)]
pub struct Writer {
    pub bits: Vec<bool>,
}

impl Writer {
    pub fn field(&mut self, value: u32, width: u32) {
        self.bits.extend((0..width).map(|n| value >> n & 1 == 1));
    }

    pub fn flag(&mut self, value: bool) {
        self.bits.push(value);
    }

    /// Bits written left first, as the tables of V.34 give frame syncs.
    pub fn pattern(&mut self, pattern: &str) {
        self.bits.extend(pattern.chars().map(|c| c == '1'));
    }
}

pub struct Reader<'a> {
    bits: &'a [bool],
}

impl<'a> Reader<'a> {
    pub fn new(bits: &'a [bool]) -> Self {
        Self { bits }
    }

    pub fn field(&mut self, width: usize) -> u32 {
        let (field, rest) = self.bits.split_at(width);
        self.bits = rest;
        field
            .iter()
            .rev()
            .fold(0, |value, &bit| value << 1 | u32::from(bit))
    }

    pub fn flag(&mut self) -> bool {
        self.field(1) == 1
    }
}

#[cfg(test)]
pub fn pattern(pattern: &str) -> Vec<bool> {
    pattern.chars().map(|c| c == '1').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crc_of_a_frame_and_its_crc_is_zero() {
        let data = pattern("1011001110001111010101");
        let mut writer = Writer::default();
        writer.bits.extend(&data);
        writer.field(u32::from(crc(data.iter().copied())), 16);
        assert_eq!(crc(writer.bits), 0);
    }

    #[test]
    fn the_crc_of_nothing_is_its_preset() {
        assert_eq!(crc([]), 0xFFFF);
    }

    #[test]
    fn reads_what_it_writes() {
        let mut writer = Writer::default();
        writer.field(0b101, 3);
        writer.flag(true);
        writer.field(300, 10);
        let mut reader = Reader::new(&writer.bits);
        assert_eq!(
            (reader.field(3), reader.flag(), reader.field(10)),
            (0b101, true, 300)
        );
    }

    #[test]
    fn writes_the_least_significant_bit_first() {
        let mut writer = Writer::default();
        writer.field(0b0001, 4);
        assert_eq!(writer.bits, pattern("1000"));
    }
}
