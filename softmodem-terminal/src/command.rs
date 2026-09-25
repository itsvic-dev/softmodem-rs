// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Parses the text of an AT command line, after the `AT`.

use std::ops::RangeInclusive;
use std::str::FromStr;

use crate::settings::Carrier;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Answer,
    Dial(Dial),
    Echo(bool),
    OffHook(bool),
    Identify(u8),
    Loudness(u8),
    Monitor(u8),
    /// `O`, and with `O1` a retrain as it returns to data mode.
    Online {
        retrain: bool,
    },
    Quiet(bool),
    Verbose(bool),
    ResultSet(u8),
    Reset,
    FactoryReset,
    /// `&C1` makes DCD follow carrier, `&C0` keeps it on.
    CarrierDetect(bool),
    SetRegister {
        register: u8,
        value: u8,
    },
    ReadRegister(u8),
    /// `+MS=`, the highest modulation for the next call, whether automode
    /// may fall back from it, and the highest rate to transmit at. Automode
    /// is on unless the command turns it off.
    SetCarrier {
        carrier: Carrier,
        automode: bool,
        max_transmit: Option<u32>,
    },
    /// `+MS?`.
    ReadCarrier,
    /// `+MS=?`.
    ListCarriers,
    /// `+ES=`, or `\N` for all three: how to try V.42 error control. A
    /// subparameter left out keeps its value.
    SetErrorControl {
        orig_rqst: Option<u8>,
        orig_fbk: Option<u8>,
        ans_fbk: Option<u8>,
    },
    /// `+ES?`.
    ReadErrorControl,
    /// `+ES=?`.
    ListErrorControl,
    /// `+ER=`, whether to report the error control in use before CONNECT.
    ReportErrorControl(bool),
    /// `+ER?`.
    ReadErrorReport,
    /// `+ER=?`.
    ListErrorReport,
    /// `+DS=`, or `%C` for the direction: how to ask for V.42 bis. A
    /// subparameter left out keeps its value.
    SetCompression {
        direction: Option<u8>,
        required: Option<bool>,
        max_dict: Option<u16>,
        max_string: Option<u8>,
    },
    /// `+DS?`.
    ReadCompression,
    /// `+DS=?`.
    ListCompression,
    /// `+DR=`, whether to report the compression in use before CONNECT.
    ReportCompression(bool),
    /// `+DR?`.
    ReadCompressionReport,
    /// `+DR=?`.
    ListCompressionReport,
    /// An extended or vendor command, accepted without effect.
    Ignored,
}

/// A dial string with its modifiers taken out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dial {
    pub number: String,
    pub pauses: u32,
    pub reverse: bool,
    pub stay_in_command_mode: bool,
    pub redial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseError;

/// Parses everything after `AT`. Spaces are ignored and case does not
/// matter, as on a Hayes modem.
///
/// # Errors
///
/// Fails on an unknown command or a value out of range, which the modem
/// reports as `ERROR` without running any of the line.
pub fn parse(line: &[u8]) -> Result<Vec<Command>, ParseError> {
    let text: Vec<u8> = line
        .iter()
        .filter(|b| **b != b' ')
        .map(u8::to_ascii_uppercase)
        .collect();
    let mut parser = Parser { text: &text, at: 0 };
    let mut commands = Vec::new();
    while let Some(letter) = parser.next() {
        commands.push(parser.command(letter)?);
    }
    Ok(commands)
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn next(&mut self) -> Option<u8> {
        let byte = self.text.get(self.at).copied();
        self.at += usize::from(byte.is_some());
        byte
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn number<T: FromStr>(&mut self) -> Result<Option<T>, ParseError> {
        let start = self.at;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.at += 1;
        }
        if start == self.at {
            return Ok(None);
        }
        std::str::from_utf8(&self.text[start..self.at])
            .ok()
            .and_then(|digits| digits.parse().ok())
            .map(Some)
            .ok_or(ParseError)
    }

    fn value(&mut self, max: u8) -> Result<u8, ParseError> {
        let value = self.number()?.unwrap_or(0);
        if value > max {
            return Err(ParseError);
        }
        Ok(value)
    }

    fn command(&mut self, letter: u8) -> Result<Command, ParseError> {
        Ok(match letter {
            b'A' => {
                self.value(0)?;
                Command::Answer
            }
            b'D' => Command::Dial(self.dial()?),
            b'E' => Command::Echo(self.value(1)? == 1),
            b'H' => Command::OffHook(self.value(1)? == 1),
            b'I' => Command::Identify(self.value(9)?),
            b'L' => Command::Loudness(self.value(3)?),
            b'M' => Command::Monitor(self.value(2)?),
            b'O' => Command::Online {
                retrain: self.value(1)? == 1,
            },
            b'Q' => Command::Quiet(self.value(1)? == 1),
            b'V' => Command::Verbose(self.value(1)? == 1),
            b'X' => Command::ResultSet(self.value(4)?),
            b'Z' => {
                self.value(1)?;
                Command::Reset
            }
            b'S' => self.register()?,
            b'B' | b'C' | b'N' | b'P' | b'T' | b'W' | b'Y' => {
                self.number::<u8>()?;
                Command::Ignored
            }
            b'&' if self.peek() == Some(b'C') => {
                self.at += 1;
                Command::CarrierDetect(self.value(1)? == 1)
            }
            b'&' if self.peek() == Some(b'F') => {
                self.at += 1;
                self.value(0)?;
                Command::FactoryReset
            }
            b'\\' if self.peek() == Some(b'N') => {
                self.at += 1;
                let (orig_rqst, orig_fbk, ans_fbk) = match self.value(3)? {
                    0 | 1 => (1, 0, 1),
                    2 => (3, 3, 5),
                    _ => (3, 0, 2),
                };
                Command::SetErrorControl {
                    orig_rqst: Some(orig_rqst),
                    orig_fbk: Some(orig_fbk),
                    ans_fbk: Some(ans_fbk),
                }
            }
            b'%' if self.peek() == Some(b'C') => {
                self.at += 1;
                Command::SetCompression {
                    direction: Some(if self.value(3)? == 0 { 0 } else { 3 }),
                    required: None,
                    max_dict: None,
                    max_string: None,
                }
            }
            b'&' | b'\\' | b'%' => {
                self.next()
                    .filter(u8::is_ascii_alphabetic)
                    .ok_or(ParseError)?;
                self.number::<u8>()?;
                Command::Ignored
            }
            b'+' => {
                let start = self.at;
                while self.peek().is_some_and(|b| b.is_ascii_alphanumeric()) {
                    self.at += 1;
                }
                match &self.text[start..self.at] {
                    b"MS" => self.modulation()?,
                    b"ES" => self.error_control()?,
                    b"ER" => self.error_report()?,
                    b"DS" => self.compression()?,
                    b"DR" => self.compression_report()?,
                    _ => {
                        self.skip_extended();
                        Command::Ignored
                    }
                }
            }
            _ => return Err(ParseError),
        })
    }

    fn skip_extended(&mut self) {
        while self.next().is_some_and(|b| b != b';') {}
    }

    fn end_extended(&mut self, command: Command) -> Result<Command, ParseError> {
        match self.next() {
            None | Some(b';') => Ok(command),
            Some(_) => Err(ParseError),
        }
    }

    /// Up to `N` comma-separated values, each in its range.
    fn subparameters<const N: usize>(
        &mut self,
        ranges: [RangeInclusive<u16>; N],
    ) -> Result<[Option<u16>; N], ParseError> {
        let mut values = [None; N];
        for (i, (value, range)) in values.iter_mut().zip(ranges).enumerate() {
            if i > 0 {
                if self.peek() != Some(b',') {
                    break;
                }
                self.at += 1;
            }
            *value = self.number()?;
            if value.is_some_and(|v| !range.contains(&v)) {
                return Err(ParseError);
            }
        }
        Ok(values)
    }

    fn error_control(&mut self) -> Result<Command, ParseError> {
        let command = match (self.next(), self.peek()) {
            (Some(b'?'), _) => Command::ReadErrorControl,
            (Some(b'='), Some(b'?')) => {
                self.at += 1;
                Command::ListErrorControl
            }
            (Some(b'='), _) => {
                let [orig_rqst, orig_fbk, ans_fbk] =
                    self.subparameters([0..=3, 0..=3, 0..=5])?.map(small);
                Command::SetErrorControl {
                    orig_rqst,
                    orig_fbk,
                    ans_fbk,
                }
            }
            _ => return Err(ParseError),
        };
        self.end_extended(command)
    }

    fn compression(&mut self) -> Result<Command, ParseError> {
        let command = match (self.next(), self.peek()) {
            (Some(b'?'), _) => Command::ReadCompression,
            (Some(b'='), Some(b'?')) => {
                self.at += 1;
                Command::ListCompression
            }
            (Some(b'='), _) => {
                let [direction, negotiation, max_dict, max_string] =
                    self.subparameters([0..=3, 0..=1, 512..=u16::MAX, 6..=250])?;
                Command::SetCompression {
                    direction: small(direction),
                    required: negotiation.map(|n| n == 1),
                    max_dict,
                    max_string: small(max_string),
                }
            }
            _ => return Err(ParseError),
        };
        self.end_extended(command)
    }

    fn compression_report(&mut self) -> Result<Command, ParseError> {
        let command = match (self.next(), self.peek()) {
            (Some(b'?'), _) => Command::ReadCompressionReport,
            (Some(b'='), Some(b'?')) => {
                self.at += 1;
                Command::ListCompressionReport
            }
            (Some(b'='), _) => Command::ReportCompression(self.value(1)? == 1),
            _ => return Err(ParseError),
        };
        self.end_extended(command)
    }

    fn error_report(&mut self) -> Result<Command, ParseError> {
        let command = match (self.next(), self.peek()) {
            (Some(b'?'), _) => Command::ReadErrorReport,
            (Some(b'='), Some(b'?')) => {
                self.at += 1;
                Command::ListErrorReport
            }
            (Some(b'='), _) => Command::ReportErrorControl(self.value(1)? == 1),
            _ => return Err(ParseError),
        };
        self.end_extended(command)
    }

    fn modulation(&mut self) -> Result<Command, ParseError> {
        let command = match (self.next(), self.peek()) {
            (Some(b'?'), _) => Command::ReadCarrier,
            (Some(b'='), Some(b'?')) => {
                self.at += 1;
                Command::ListCarriers
            }
            (Some(b'='), _) => {
                let start = self.at;
                while self.peek().is_some_and(|b| b.is_ascii_alphanumeric()) {
                    self.at += 1;
                }
                let name = &self.text[start..self.at];
                let carrier = Carrier::ALL
                    .into_iter()
                    .find(|c| c.name().as_bytes() == name)
                    .ok_or(ParseError)?;
                let mut automode = true;
                if self.peek() == Some(b',') {
                    self.at += 1;
                    automode = match self.number()? {
                        None | Some(1) => true,
                        Some(0) => false,
                        Some(_) => return Err(ParseError),
                    };
                }
                // <min_tx_rate>, <max_tx_rate>, <min_rx_rate> and <max_rx_rate>, of which only the second counts.
                let mut rates = Vec::new();
                while self.peek() == Some(b',') {
                    self.at += 1;
                    rates.push(self.number::<u32>()?);
                }
                let max_transmit = rates.get(1).copied().flatten().filter(|&rate| rate > 0);
                Command::SetCarrier {
                    carrier,
                    automode,
                    max_transmit,
                }
            }
            _ => return Err(ParseError),
        };
        self.end_extended(command)
    }

    fn register(&mut self) -> Result<Command, ParseError> {
        let register = self.number()?.ok_or(ParseError)?;
        match self.next() {
            Some(b'=') => Ok(Command::SetRegister {
                register,
                value: self.number()?.unwrap_or(0),
            }),
            Some(b'?') => Ok(Command::ReadRegister(register)),
            _ => Err(ParseError),
        }
    }

    fn dial(&mut self) -> Result<Dial, ParseError> {
        let mut dial = Dial::default();
        while let Some(byte) = self.next() {
            match byte {
                b'0'..=b'9' | b'*' | b'#' | b'A'..=b'D' => dial.number.push(char::from(byte)),
                b'T' | b'P' | b'W' | b'@' | b'!' | b'-' | b'(' | b')' | b'.' => {}
                b',' => dial.pauses += 1,
                b'R' => dial.reverse = true,
                b'L' => dial.redial = true,
                b';' => {
                    dial.stay_in_command_mode = true;
                    break;
                }
                _ => return Err(ParseError),
            }
        }
        Ok(dial)
    }
}

fn small(value: Option<u16>) -> Option<u8> {
    value.and_then(|v| u8::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dial(number: &str) -> Dial {
        Dial {
            number: number.into(),
            ..Dial::default()
        }
    }

    #[test]
    fn parses_a_run_of_basic_commands() {
        assert_eq!(
            parse(b"E0 v1q0x4 H m2l3").unwrap(),
            [
                Command::Echo(false),
                Command::Verbose(true),
                Command::Quiet(false),
                Command::ResultSet(4),
                Command::OffHook(false),
                Command::Monitor(2),
                Command::Loudness(3),
            ]
        );
    }

    #[test]
    fn an_empty_line_is_valid() {
        assert_eq!(parse(b"").unwrap(), []);
    }

    #[test]
    fn reads_and_writes_registers() {
        assert_eq!(
            parse(b"S0=1S7?").unwrap(),
            [
                Command::SetRegister {
                    register: 0,
                    value: 1
                },
                Command::ReadRegister(7),
            ]
        );
    }

    #[test]
    fn rejects_values_out_of_range() {
        assert_eq!(parse(b"X5"), Err(ParseError));
        assert_eq!(parse(b"S0=256"), Err(ParseError));
        assert_eq!(parse(b"S300=1"), Err(ParseError));
        assert_eq!(parse(b"E2"), Err(ParseError));
    }

    #[test]
    fn rejects_unknown_commands() {
        assert_eq!(parse(b"G"), Err(ParseError));
        assert_eq!(parse(b"S0"), Err(ParseError));
    }

    #[test]
    fn parses_carrier_detect_and_factory_reset() {
        assert_eq!(
            parse(b"&F&C1&C").unwrap(),
            [
                Command::FactoryReset,
                Command::CarrierDetect(true),
                Command::CarrierDetect(false),
            ]
        );
        assert_eq!(parse(b"&C2"), Err(ParseError));
    }

    #[test]
    fn accepts_vendor_commands_without_effect() {
        assert_eq!(
            parse(b"&K3&D2\\Q3%E2+FCLASS=0;+ESR=1;E1").unwrap(),
            [
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Echo(true),
            ]
        );
    }

    fn error_control(orig_rqst: u8, orig_fbk: u8, ans_fbk: u8) -> Command {
        Command::SetErrorControl {
            orig_rqst: Some(orig_rqst),
            orig_fbk: Some(orig_fbk),
            ans_fbk: Some(ans_fbk),
        }
    }

    #[test]
    fn parses_error_control() {
        assert_eq!(
            parse(b"+ES=3,0,2;+es=2;+ES=,,4;+ES?;+ES=?").unwrap(),
            [
                error_control(3, 0, 2),
                Command::SetErrorControl {
                    orig_rqst: Some(2),
                    orig_fbk: None,
                    ans_fbk: None,
                },
                Command::SetErrorControl {
                    orig_rqst: None,
                    orig_fbk: None,
                    ans_fbk: Some(4),
                },
                Command::ReadErrorControl,
                Command::ListErrorControl,
            ]
        );
        assert_eq!(
            parse(b"+ER=1;+ER=0;+ER?;+ER=?").unwrap(),
            [
                Command::ReportErrorControl(true),
                Command::ReportErrorControl(false),
                Command::ReadErrorReport,
                Command::ListErrorReport,
            ]
        );
    }

    #[test]
    fn rejects_error_control_it_cannot_run() {
        assert_eq!(parse(b"+ES=4"), Err(ParseError));
        assert_eq!(parse(b"+ES=3,4"), Err(ParseError));
        assert_eq!(parse(b"+ES=3,0,6"), Err(ParseError));
        assert_eq!(parse(b"+ER=2"), Err(ParseError));
        assert_eq!(parse(b"\\N4"), Err(ParseError));
    }

    fn compression(direction: Option<u8>) -> Command {
        Command::SetCompression {
            direction,
            required: None,
            max_dict: None,
            max_string: None,
        }
    }

    #[test]
    fn parses_data_compression() {
        assert_eq!(
            parse(b"+DS=3,1,4096,250;+DS=0;+DS?;+DS=?;+DR=1;+DR?;+DR=?").unwrap(),
            [
                Command::SetCompression {
                    direction: Some(3),
                    required: Some(true),
                    max_dict: Some(4096),
                    max_string: Some(250),
                },
                compression(Some(0)),
                Command::ReadCompression,
                Command::ListCompression,
                Command::ReportCompression(true),
                Command::ReadCompressionReport,
                Command::ListCompressionReport,
            ]
        );
        assert_eq!(
            parse(b"%C0%C1%C3").unwrap(),
            [
                compression(Some(0)),
                compression(Some(3)),
                compression(Some(3))
            ]
        );
    }

    #[test]
    fn rejects_compression_outside_v250() {
        assert_eq!(parse(b"+DS=4"), Err(ParseError));
        assert_eq!(parse(b"+DS=3,2"), Err(ParseError));
        assert_eq!(parse(b"+DS=3,0,511"), Err(ParseError));
        assert_eq!(parse(b"+DS=3,0,2048,5"), Err(ParseError));
        assert_eq!(parse(b"+DS=3,0,2048,251"), Err(ParseError));
        assert_eq!(parse(b"+DS=3,0,65536"), Err(ParseError));
        assert_eq!(parse(b"%C4"), Err(ParseError));
    }

    #[test]
    fn backslash_n_sets_all_of_es() {
        assert_eq!(
            parse(b"\\N\\N1\\N2\\N3").unwrap(),
            [
                error_control(1, 0, 1),
                error_control(1, 0, 1),
                error_control(3, 3, 5),
                error_control(3, 0, 2),
            ]
        );
    }

    #[test]
    fn o1_asks_for_a_retrain() {
        assert_eq!(
            parse(b"OO0O1").unwrap(),
            [
                Command::Online { retrain: false },
                Command::Online { retrain: false },
                Command::Online { retrain: true },
            ]
        );
        assert_eq!(parse(b"O2"), Err(ParseError));
    }

    #[test]
    fn parses_the_modulation() {
        assert_eq!(
            parse(b"+ms=v22;+MS=V21,0,300,300;+MS=V22B,1;+MS=V22,;E0").unwrap(),
            [
                Command::SetCarrier {
                    carrier: Carrier::V22,
                    automode: true,
                    max_transmit: None
                },
                Command::SetCarrier {
                    carrier: Carrier::V21,
                    automode: false,
                    max_transmit: Some(300)
                },
                Command::SetCarrier {
                    carrier: Carrier::V22bis,
                    automode: true,
                    max_transmit: None
                },
                Command::SetCarrier {
                    carrier: Carrier::V22,
                    automode: true,
                    max_transmit: None
                },
                Command::Echo(false),
            ]
        );
        assert_eq!(parse(b"+MS?").unwrap(), [Command::ReadCarrier]);
        assert_eq!(parse(b"+MS=?").unwrap(), [Command::ListCarriers]);
        assert_eq!(
            parse(b"+MS=V34,0,2400,33600").unwrap(),
            [Command::SetCarrier {
                carrier: Carrier::V34,
                automode: false,
                max_transmit: Some(33_600)
            }]
        );
        assert_eq!(
            parse(b"+MS=V90,1,,24000,0,0").unwrap(),
            [Command::SetCarrier {
                carrier: Carrier::V90,
                automode: true,
                max_transmit: Some(24_000)
            }]
        );
        assert_eq!(
            parse(b"+MS=V90,1,300,0").unwrap(),
            [Command::SetCarrier {
                carrier: Carrier::V90,
                automode: true,
                max_transmit: None
            }]
        );
    }

    #[test]
    fn rejects_modulations_it_cannot_run() {
        assert_eq!(parse(b"+MS=V32"), Err(ParseError));
        assert_eq!(parse(b"+MS=V22,2"), Err(ParseError));
        assert_eq!(parse(b"+MS=V22BIS"), Err(ParseError));
        assert_eq!(parse(b"+MS"), Err(ParseError));
        assert_eq!(parse(b"+MS=V22X"), Err(ParseError));
    }

    #[test]
    fn a_dial_string_runs_to_the_end_of_the_line() {
        assert_eq!(parse(b"DT 0300").unwrap(), [Command::Dial(dial("0300"))]);
        assert_eq!(
            parse(b"DP(030) 0-300").unwrap(),
            [Command::Dial(dial("0300300"))]
        );
    }

    #[test]
    fn dial_modifiers_are_taken_out_of_the_number() {
        let Command::Dial(parsed) = &parse(b"DT9,,W@!R*12#AD;").unwrap()[0] else {
            panic!("not a dial");
        };
        assert_eq!(
            *parsed,
            Dial {
                number: "9*12#AD".into(),
                pauses: 2,
                reverse: true,
                stay_in_command_mode: true,
                redial: false,
            }
        );
    }

    #[test]
    fn commands_may_follow_a_dial_that_stays_in_command_mode() {
        assert_eq!(
            parse(b"D0300;H").unwrap(),
            [
                Command::Dial(Dial {
                    stay_in_command_mode: true,
                    ..dial("0300")
                }),
                Command::OffHook(false),
            ]
        );
    }

    #[test]
    fn redial_takes_no_number() {
        assert_eq!(
            parse(b"DL").unwrap(),
            [Command::Dial(Dial {
                redial: true,
                ..Dial::default()
            })]
        );
    }

    #[test]
    fn rejects_characters_a_dial_string_cannot_hold() {
        assert_eq!(parse(b"D0300X"), Err(ParseError));
    }
}
