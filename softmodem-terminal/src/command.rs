//! Parses the text of an AT command line, after the `AT`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Answer,
    Dial(Dial),
    Echo(bool),
    OffHook(bool),
    Identify(u8),
    Loudness(u8),
    Monitor(u8),
    Online,
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

    fn number(&mut self) -> Result<Option<u8>, ParseError> {
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
            b'O' => {
                self.value(1)?;
                Command::Online
            }
            b'Q' => Command::Quiet(self.value(1)? == 1),
            b'V' => Command::Verbose(self.value(1)? == 1),
            b'X' => Command::ResultSet(self.value(4)?),
            b'Z' => {
                self.value(1)?;
                Command::Reset
            }
            b'S' => self.register()?,
            b'B' | b'C' | b'N' | b'P' | b'T' | b'W' | b'Y' => {
                self.number()?;
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
            b'&' | b'\\' | b'%' => {
                self.next()
                    .filter(u8::is_ascii_alphabetic)
                    .ok_or(ParseError)?;
                self.number()?;
                Command::Ignored
            }
            b'+' => {
                while self.next().is_some_and(|b| b != b';') {}
                Command::Ignored
            }
            _ => return Err(ParseError),
        })
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
            parse(b"&K3&D2\\N0%C0+MS=V21;E1").unwrap(),
            [
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Ignored,
                Command::Echo(true),
            ]
        );
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
