// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.42 bis data compression: the dictionary, the encoder and the decoder.

use std::collections::HashMap;

use crate::xid::Compression;

/// N6, the control codewords before the first character's.
const CONTROL_CODEWORDS: u16 = 3;
/// N5, the first codeword that holds a string of two or more characters.
const FIRST_STRING: u16 = CONTROL_CODEWORDS + 256;
const NONE: u16 = u16::MAX;

const ETM: u16 = 0;
const FLUSH: u16 = 1;
const STEPUP: u16 = 2;

const ECM: u8 = 0;
const EID: u8 = 1;
const RESET: u8 = 2;
const ESCAPE_STEP: u8 = 51;

/// Characters in each window of the compressibility test (§ 7.8).
const TEST_WINDOW: u32 = 512;

/// What the two ends agree on (§ 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameters {
    /// N2, the total number of codewords, from 512.
    pub codewords: u16,
    /// N7, the longest string, from 6 to 250.
    pub max_string: u8,
}

impl Parameters {
    pub const MIN_CODEWORDS: u16 = 512;
    pub const MIN_STRING: u8 = 6;
    pub const MAX_STRING: u8 = 250;

    /// Whether § 5.1 allows these values.
    #[must_use]
    pub fn valid(self) -> bool {
        self.codewords >= Self::MIN_CODEWORDS
            && (Self::MIN_STRING..=Self::MAX_STRING).contains(&self.max_string)
    }

    /// N1, the longest codeword in bits.
    fn max_bits(self) -> u32 {
        u16::BITS - (self.codewords - 1).leading_zeros()
    }
}

const DEFAULT: Parameters = Parameters {
    codewords: Parameters::MIN_CODEWORDS,
    max_string: Parameters::MIN_STRING,
};
const FROM_INITIATOR: u8 = 1;
const TO_INITIATOR: u8 = 2;

/// Directions and parameters, from this end's point of view: what it offers
/// before negotiation, or what applies after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Directions {
    pub transmit: bool,
    pub receive: bool,
    pub parameters: Parameters,
}

/// A P1 or P2 outside what § 5.1 allows, which ends the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProceduralError;

impl Directions {
    /// P0 to P2 as the negotiation initiator sends them.
    #[must_use]
    pub fn proposal(self) -> Compression {
        Compression {
            directions: u8::from(self.transmit) * FROM_INITIATOR
                + u8::from(self.receive) * TO_INITIATOR,
            codewords: Some(self.parameters.codewords),
            max_string: Some(self.parameters.max_string),
        }
    }

    /// The responder's reply to `theirs`, and what then applies.
    ///
    /// # Errors
    ///
    /// Fails if `theirs` holds values outside § 5.1.
    pub fn answer(
        offer: Option<Self>,
        theirs: Compression,
    ) -> Result<(Compression, Option<Self>), ProceduralError> {
        let proposed = received(theirs)?;
        let ours = offer.map_or(0, |offer| {
            u8::from(offer.receive) * FROM_INITIATOR + u8::from(offer.transmit) * TO_INITIATOR
        });
        let directions = theirs.directions & ours;
        let Some(offer) = offer.filter(|_| directions != 0) else {
            let none = Compression {
                directions: 0,
                codewords: None,
                max_string: None,
            };
            return Ok((none, None));
        };
        let parameters = lower(proposed, offer.parameters);
        let reply = Compression {
            directions,
            codewords: Some(parameters.codewords),
            max_string: Some(parameters.max_string),
        };
        let agreed = Self {
            transmit: directions & TO_INITIATOR != 0,
            receive: directions & FROM_INITIATOR != 0,
            parameters,
        };
        Ok((reply, Some(agreed)))
    }

    /// What applies once the responder has replied, if anything.
    ///
    /// # Errors
    ///
    /// Fails if the reply holds values outside § 5.1, or a direction that
    /// was not proposed.
    pub fn conclude(self, reply: Option<Compression>) -> Result<Option<Self>, ProceduralError> {
        let Some(reply) = reply.filter(|r| r.directions != 0) else {
            return Ok(None);
        };
        if reply.directions & !self.proposal().directions != 0 {
            return Err(ProceduralError);
        }
        let parameters = lower(received(reply)?, self.parameters);
        Ok(Some(Self {
            transmit: reply.directions & FROM_INITIATOR != 0,
            receive: reply.directions & TO_INITIATOR != 0,
            parameters,
        }))
    }
}

fn received(compression: Compression) -> Result<Parameters, ProceduralError> {
    let parameters = Parameters {
        codewords: compression.codewords.unwrap_or(DEFAULT.codewords),
        max_string: compression.max_string.unwrap_or(DEFAULT.max_string),
    };
    if parameters.valid() {
        Ok(parameters)
    } else {
        Err(ProceduralError)
    }
}

fn lower(a: Parameters, b: Parameters) -> Parameters {
    Parameters {
        codewords: a.codewords.min(b.codewords),
        max_string: a.max_string.min(b.max_string),
    }
}

/// Why the decoder stopped (§ 5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    StepupPastMaximum,
    CodewordIsNextEmpty,
    EmptyEntry,
    ReservedCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Transparent,
    Compressed,
}

/// The strings both ends know, as a tree for each character (§ 6).
#[derive(Debug)]
struct Dictionary {
    parameters: Parameters,
    parent: Vec<u16>,
    character: Vec<u8>,
    /// The string's length, 0 for an empty entry.
    length: Vec<u8>,
    children: Vec<u16>,
    lookup: HashMap<(u16, u8), u16>,
    /// C1, the next empty entry.
    next: u16,
}

impl Dictionary {
    fn new(parameters: Parameters) -> Self {
        let n = usize::from(parameters.codewords);
        let mut dictionary = Self {
            parameters,
            parent: vec![NONE; n],
            character: vec![0; n],
            length: vec![0; n],
            children: vec![0; n],
            lookup: HashMap::new(),
            next: FIRST_STRING,
        };
        for character in 0..=u8::MAX {
            let code = usize::from(root(character));
            dictionary.character[code] = character;
            dictionary.length[code] = 1;
        }
        dictionary
    }

    fn child(&self, node: u16, character: u8) -> Option<u16> {
        self.lookup.get(&(node, character)).copied()
    }

    /// The new entry's codeword, after §§ 6.4 and 6.5.
    fn add(&mut self, node: u16, character: u8) -> Option<u16> {
        let length = self.length[usize::from(node)];
        if length >= self.parameters.max_string || self.child(node, character).is_some() {
            return None;
        }
        let code = self.next;
        let at = usize::from(code);
        self.parent[at] = node;
        self.character[at] = character;
        self.length[at] = length + 1;
        self.children[usize::from(node)] += 1;
        self.lookup.insert((node, character), code);
        self.recover();
        Some(code)
    }

    fn recover(&mut self) {
        loop {
            self.next += 1;
            if self.next >= self.parameters.codewords {
                self.next = FIRST_STRING;
            }
            let at = usize::from(self.next);
            if self.length[at] == 0 {
                return;
            }
            if self.children[at] == 0 {
                let parent = self.parent[at];
                self.lookup.remove(&(parent, self.character[at]));
                self.children[usize::from(parent)] -= 1;
                self.length[at] = 0;
                return;
            }
        }
    }

    /// The characters of the string with codeword `code`, if it is in use.
    fn string(&self, code: u16) -> Option<Vec<u8>> {
        if code < CONTROL_CODEWORDS || self.length.get(usize::from(code)).is_none_or(|&l| l == 0) {
            return None;
        }
        let mut string = Vec::with_capacity(usize::from(self.length[usize::from(code)]));
        let mut node = code;
        while node != NONE {
            string.push(self.character[usize::from(node)]);
            node = self.parent[usize::from(node)];
        }
        string.reverse();
        Some(string)
    }
}

fn root(character: u8) -> u16 {
    CONTROL_CODEWORDS + u16::from(character)
}

/// The string matching procedure (§ 6.3) as both ends run it on characters.
#[derive(Debug, Default)]
struct Matcher {
    string: Option<u16>,
    last_created: Option<u16>,
    /// The next character ends the string, after a flush or a mode change.
    exception: bool,
}

impl Matcher {
    /// Takes one character, and returns the string it ended, if it ended one.
    fn push(&mut self, dictionary: &mut Dictionary, character: u8) -> Option<u16> {
        let ended = match self.string {
            Some(string) if self.exception => {
                self.last_created = dictionary.add(string, character);
                None
            }
            Some(string) => match dictionary.child(string, character) {
                Some(child) if Some(child) != self.last_created => {
                    self.string = Some(child);
                    return None;
                }
                _ => {
                    self.last_created = dictionary.add(string, character);
                    Some(string)
                }
            },
            None => None,
        };
        self.exception = false;
        self.string = Some(root(character));
        ended
    }
}

/// Packs codewords of varying width, least significant bit first (§ 7.5).
#[derive(Debug, Default)]
struct BitWriter {
    bits: u32,
    count: u32,
    out: Vec<u8>,
}

impl BitWriter {
    fn put(&mut self, value: u16, width: u32) {
        self.bits |= u32::from(value) << self.count;
        self.count += width;
        while self.count >= 8 {
            self.out.push(self.bits.to_le_bytes()[0]);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    fn aligned(&self) -> bool {
        self.count == 0
    }

    fn align(&mut self) {
        if self.count > 0 {
            self.put(0, 8 - self.count);
        }
    }
}

/// Compresses the DTE's data for one direction.
#[derive(Debug)]
pub struct Encoder {
    parameters: Parameters,
    dictionary: Dictionary,
    matcher: Matcher,
    mode: Mode,
    escape: u8,
    /// C2, the codeword size in bits.
    width: u32,
    writer: BitWriter,
    window_characters: u32,
    window_bits: u32,
}

impl Encoder {
    #[must_use]
    pub fn new(parameters: Parameters) -> Self {
        Self {
            parameters,
            dictionary: Dictionary::new(parameters),
            matcher: Matcher::default(),
            mode: Mode::Transparent,
            escape: 0,
            width: 9,
            writer: BitWriter::default(),
            window_characters: 0,
            window_bits: 0,
        }
    }

    /// Whether the encoder is in compressed mode.
    #[must_use]
    pub fn compressing(&self) -> bool {
        self.mode == Mode::Compressed
    }

    /// Encodes `data`, and returns the octets completed so far.
    pub fn encode(&mut self, data: &[u8]) -> Vec<u8> {
        for &character in data {
            self.character(character);
        }
        std::mem::take(&mut self.writer.out)
    }

    /// Sends what is held back, for when the link would go idle (§ 7.9).
    pub fn flush(&mut self) -> Vec<u8> {
        if self.mode == Mode::Compressed
            && !self.matcher.exception
            && let Some(string) = self.matcher.string
        {
            self.codeword(string);
            self.matcher.exception = true;
            if !self.writer.aligned() {
                self.put(FLUSH);
                self.writer.align();
            }
        }
        std::mem::take(&mut self.writer.out)
    }

    fn character(&mut self, character: u8) {
        let ended = self.matcher.push(&mut self.dictionary, character);
        match self.mode {
            Mode::Compressed => {
                if let Some(string) = ended {
                    self.window_bits += self.codeword(string);
                }
            }
            Mode::Transparent => {
                if let Some(string) = ended {
                    self.window_bits += self.width_for(string);
                }
                self.writer.out.push(character);
                if character == self.escape {
                    self.writer.out.push(EID);
                }
            }
        }
        if character == self.escape {
            self.escape = self.escape.wrapping_add(ESCAPE_STEP);
        }
        self.window_characters += 1;
        if self.window_characters == TEST_WINDOW {
            self.test_compressibility();
        }
    }

    /// Compressed if it would have saved a quarter, transparent if it saved nothing.
    fn test_compressibility(&mut self) {
        let raw = self.window_characters * 8;
        let bits = self.window_bits;
        self.window_characters = 0;
        self.window_bits = 0;
        match self.mode {
            Mode::Transparent if bits * 4 < raw * 3 => {
                self.writer.out.extend([self.escape, ECM]);
                self.mode = Mode::Compressed;
                self.matcher.exception = true;
            }
            Mode::Compressed if bits > raw => {
                if let Some(string) = self.matcher.string
                    && !self.matcher.exception
                {
                    self.codeword(string);
                }
                self.put(ETM);
                self.writer.align();
                self.mode = Mode::Transparent;
                self.matcher.exception = true;
            }
            _ => {}
        }
    }

    fn width_for(&self, code: u16) -> u32 {
        self.width.max(u16::BITS - code.leading_zeros())
    }

    /// The bits it took to send `code` and the STEPUPs before it (§ 7.4).
    fn codeword(&mut self, code: u16) -> u32 {
        let mut bits = 0;
        while u32::from(code) >= 1 << self.width {
            self.put(STEPUP);
            bits += self.width;
            self.width += 1;
        }
        debug_assert!(self.width <= self.parameters.max_bits());
        self.put(code);
        bits + self.width
    }

    fn put(&mut self, code: u16) {
        self.writer.put(code, self.width);
    }
}

/// Recovers the far end's data for one direction.
#[derive(Debug)]
pub struct Decoder {
    parameters: Parameters,
    dictionary: Dictionary,
    matcher: Matcher,
    mode: Mode,
    escape: u8,
    escaped: bool,
    width: u32,
    bits: u32,
    count: u32,
    previous: Option<u16>,
}

impl Decoder {
    #[must_use]
    pub fn new(parameters: Parameters) -> Self {
        Self {
            parameters,
            dictionary: Dictionary::new(parameters),
            matcher: Matcher::default(),
            mode: Mode::Transparent,
            escape: 0,
            escaped: false,
            width: 9,
            bits: 0,
            count: 0,
            previous: None,
        }
    }

    /// Decodes octets from the far end, in order, appending the data to `out`.
    ///
    /// # Errors
    ///
    /// Fails on what § 5.8 calls a C-ERROR, after which the decoder is out
    /// of step with the far end.
    pub fn decode(&mut self, octets: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        for &octet in octets {
            match self.mode {
                Mode::Transparent => self.transparent(octet, out)?,
                Mode::Compressed => {
                    self.bits |= u32::from(octet) << self.count;
                    self.count += 8;
                    while self.mode == Mode::Compressed && self.count >= self.width {
                        let code = self.bits & ((1 << self.width) - 1);
                        self.bits >>= self.width;
                        self.count -= self.width;
                        #[expect(clippy::cast_possible_truncation, reason = "at most 16 bits")]
                        self.codeword(code as u16, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn transparent(&mut self, octet: u8, out: &mut Vec<u8>) -> Result<(), Error> {
        if std::mem::take(&mut self.escaped) {
            return match octet {
                ECM => {
                    self.mode = Mode::Compressed;
                    self.previous = self.matcher.string;
                    self.bits = 0;
                    self.count = 0;
                    Ok(())
                }
                EID => {
                    let character = self.escape;
                    self.matcher.push(&mut self.dictionary, character);
                    out.push(character);
                    self.escape = self.escape.wrapping_add(ESCAPE_STEP);
                    Ok(())
                }
                RESET => {
                    *self = Self::new(self.parameters);
                    Ok(())
                }
                _ => Err(Error::ReservedCommand),
            };
        }
        if octet == self.escape {
            self.escaped = true;
        } else {
            self.matcher.push(&mut self.dictionary, octet);
            out.push(octet);
        }
        Ok(())
    }

    fn codeword(&mut self, code: u16, out: &mut Vec<u8>) -> Result<(), Error> {
        match code {
            ETM => {
                self.align();
                self.mode = Mode::Transparent;
                self.matcher = Matcher {
                    string: self.previous,
                    last_created: None,
                    exception: true,
                };
            }
            FLUSH => self.align(),
            STEPUP => {
                self.width += 1;
                if self.width > self.parameters.max_bits() {
                    return Err(Error::StepupPastMaximum);
                }
            }
            code => {
                if code == self.dictionary.next {
                    return Err(Error::CodewordIsNextEmpty);
                }
                let string = self.dictionary.string(code).ok_or(Error::EmptyEntry)?;
                if let Some(previous) = self.previous {
                    self.dictionary.add(previous, string[0]);
                }
                self.previous = Some(code);
                for character in string {
                    out.push(character);
                    if character == self.escape {
                        self.escape = self.escape.wrapping_add(ESCAPE_STEP);
                    }
                }
            }
        }
        Ok(())
    }

    fn align(&mut self) {
        let drop = self.count % 8;
        self.bits >>= drop;
        self.count -= drop;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: Parameters = Parameters {
        codewords: 512,
        max_string: 6,
    };
    const LARGE: Parameters = Parameters {
        codewords: 2048,
        max_string: 32,
    };

    fn text(len: usize) -> Vec<u8> {
        let words = [
            "the ", "modem ", "sends ", "a ", "carrier ", "and ", "data ", "over ", "line ", "to ",
            "ppp\r\n",
        ];
        let mut seed = 7u32;
        let mut out = Vec::new();
        while out.len() < len {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            out.extend(words[(seed >> 16) as usize % words.len()].bytes());
        }
        out.truncate(len);
        out
    }

    fn noise(len: usize) -> Vec<u8> {
        let mut seed = 1u32;
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed.to_le_bytes()[0]
            })
            .collect()
    }

    /// Encodes `chunks` with a flush after each, and decodes the lot.
    fn round_trip(parameters: Parameters, chunks: &[Vec<u8>]) -> (Vec<u8>, usize) {
        let mut encoder = Encoder::new(parameters);
        let mut decoder = Decoder::new(parameters);
        let mut wire = Vec::new();
        for chunk in chunks {
            wire.extend(encoder.encode(chunk));
            wire.extend(encoder.flush());
        }
        let mut out = Vec::new();
        decoder.decode(&wire, &mut out).unwrap();
        (out, wire.len())
    }

    #[test]
    fn text_round_trips_and_shrinks() {
        let data = text(20_000);
        let (out, sent) = round_trip(LARGE, std::slice::from_ref(&data));
        assert_eq!(out, data);
        assert!(sent * 2 < data.len(), "{sent} octets for {}", data.len());
    }

    #[test]
    fn a_small_dictionary_recovers_entries_and_stays_in_step() {
        let data = [text(10_000), noise(3000), text(10_000)].concat();
        let (out, _) = round_trip(SMALL, std::slice::from_ref(&data));
        assert_eq!(out, data);
    }

    #[test]
    fn noise_goes_transparent_and_barely_grows() {
        let data = noise(20_000);
        let (out, sent) = round_trip(LARGE, std::slice::from_ref(&data));
        assert_eq!(out, data);
        assert!(
            sent < data.len() + data.len() / 20,
            "{sent} octets for {}",
            data.len()
        );
    }

    #[test]
    fn switches_between_modes_and_back() {
        let data = [text(5000), noise(5000), text(5000), noise(5000)].concat();
        let mut encoder = Encoder::new(LARGE);
        let mut decoder = Decoder::new(LARGE);
        let mut modes = Vec::new();
        let mut out = Vec::new();
        for chunk in data.chunks(100) {
            let wire = encoder.encode(chunk);
            decoder.decode(&wire, &mut out).unwrap();
            if modes.last() != Some(&encoder.compressing()) {
                modes.push(encoder.compressing());
            }
        }
        decoder.decode(&encoder.flush(), &mut out).unwrap();
        assert_eq!(out, data);
        assert_eq!(modes, [false, true, false, true, false]);
    }

    #[test]
    fn flushes_between_short_writes_keep_step() {
        let chunks: Vec<Vec<u8>> = text(8000).chunks(37).map(<[u8]>::to_vec).collect();
        let (out, _) = round_trip(LARGE, &chunks);
        assert_eq!(out, chunks.concat());
    }

    #[test]
    fn escape_characters_in_data_are_carried() {
        let data: Vec<u8> = (0..4000).map(|i| [0u8, 51, 102, b'x'][i % 4]).collect();
        let (out, _) = round_trip(SMALL, std::slice::from_ref(&data));
        assert_eq!(out, data);
    }

    #[test]
    fn repeated_characters_do_not_use_an_entry_before_the_decoder_has_it() {
        let data = vec![b'C'; 5000];
        let (out, sent) = round_trip(LARGE, std::slice::from_ref(&data));
        assert_eq!(out, data);
        assert!(sent < 1000);
    }

    #[test]
    fn follows_the_matching_example_of_appendix_ii_4_3() {
        let mut dictionary = Dictionary::new(LARGE);
        let mut matcher = Matcher::default();
        let ended: Vec<Option<u16>> = b"CCCCC"
            .iter()
            .map(|&c| matcher.push(&mut dictionary, c))
            .collect();
        let c = root(b'C');
        assert_eq!(ended, [None, Some(c), Some(c), None, Some(FIRST_STRING)]);
    }

    #[test]
    fn the_decoder_refuses_what_section_5_8_calls_errors() {
        let mut out = Vec::new();
        let mut decoder = Decoder::new(SMALL);
        assert_eq!(
            decoder.decode(&[0, 7], &mut out),
            Err(Error::ReservedCommand)
        );
        let compressed = |codes: &[u16]| {
            let mut writer = BitWriter::default();
            for &code in codes {
                writer.put(code, 9);
            }
            writer.align();
            [vec![0, ECM], writer.out].concat()
        };
        let mut decoder = Decoder::new(SMALL);
        assert_eq!(
            decoder.decode(&compressed(&[STEPUP]), &mut out),
            Err(Error::StepupPastMaximum)
        );
        let mut decoder = Decoder::new(SMALL);
        assert_eq!(
            decoder.decode(&compressed(&[FIRST_STRING]), &mut out),
            Err(Error::CodewordIsNextEmpty)
        );
        let mut decoder = Decoder::new(SMALL);
        assert_eq!(
            decoder.decode(&compressed(&[root(b'a'), FIRST_STRING + 1]), &mut out),
            Err(Error::EmptyEntry)
        );
    }

    #[test]
    fn packs_codewords_least_significant_bit_first() {
        let mut writer = BitWriter::default();
        writer.put(0x1ab, 9);
        writer.put(0x0cd, 9);
        writer.align();
        assert_eq!(writer.out, [0xab, 0x9b, 0x01]);
    }

    fn offer(transmit: bool, receive: bool, parameters: Parameters) -> Directions {
        Directions {
            transmit,
            receive,
            parameters,
        }
    }

    #[test]
    fn both_ends_take_the_lower_values_and_the_common_directions() {
        let initiator = offer(true, true, LARGE);
        let responder = offer(false, true, SMALL);
        let (reply, agreed) = Directions::answer(Some(responder), initiator.proposal()).unwrap();
        assert_eq!(agreed, Some(offer(false, true, SMALL)));
        assert_eq!(reply.directions, FROM_INITIATOR);
        assert_eq!(
            initiator.conclude(Some(reply)),
            Ok(Some(offer(true, false, SMALL)))
        );
        let (reply, agreed) =
            Directions::answer(Some(offer(true, true, SMALL)), initiator.proposal()).unwrap();
        assert_eq!(agreed, Some(offer(true, true, SMALL)));
        assert_eq!(
            initiator.conclude(Some(reply)),
            Ok(Some(offer(true, true, SMALL)))
        );
    }

    #[test]
    fn no_offer_or_no_reply_means_no_compression() {
        let initiator = offer(true, true, LARGE);
        let (reply, agreed) = Directions::answer(None, initiator.proposal()).unwrap();
        assert_eq!((reply.directions, agreed), (0, None));
        assert_eq!(initiator.conclude(Some(reply)), Ok(None));
        assert_eq!(initiator.conclude(None), Ok(None));
    }

    #[test]
    fn values_outside_section_5_1_are_procedural_errors() {
        let bad = Compression {
            directions: 3,
            codewords: Some(300),
            max_string: None,
        };
        assert_eq!(
            Directions::answer(Some(offer(true, true, LARGE)), bad),
            Err(ProceduralError)
        );
        let only_transmit = offer(true, false, LARGE);
        let reply = Compression {
            directions: 3,
            codewords: None,
            max_string: None,
        };
        assert_eq!(only_transmit.conclude(Some(reply)), Err(ProceduralError));
    }

    #[test]
    fn negotiated_limits() {
        assert_eq!(SMALL.max_bits(), 9);
        assert_eq!(LARGE.max_bits(), 11);
        let short = Parameters {
            codewords: 511,
            max_string: 6,
        };
        let long = Parameters {
            codewords: 512,
            max_string: 251,
        };
        assert!(!short.valid() && !long.valid() && LARGE.valid());
    }
}
