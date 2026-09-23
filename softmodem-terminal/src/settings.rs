//! What the AT commands set, and how the modem answers the computer.

use std::time::Duration;

use crate::command::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    Ok,
    Connect,
    Ring,
    NoCarrier,
    Error,
    NoDialtone,
    Busy,
}

impl ResultCode {
    fn digit(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Connect => 1,
            Self::Ring => 2,
            Self::NoCarrier => 3,
            Self::Error => 4,
            Self::NoDialtone => 6,
            Self::Busy => 7,
        }
    }

    fn words(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Connect => "CONNECT",
            Self::Ring => "RING",
            Self::NoCarrier => "NO CARRIER",
            Self::Error => "ERROR",
            Self::NoDialtone => "NO DIALTONE",
            Self::Busy => "BUSY",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub echo: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub result_set: u8,
    pub loudness: u8,
    pub monitor: u8,
    pub dcd: Dcd,
    pub registers: [u8; 256],
}

/// What DCD shows the computer, set by `&C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dcd {
    /// `&C0`, the Hayes default.
    AlwaysOn,
    /// `&C1`.
    FollowsCarrier,
}

impl Default for Settings {
    fn default() -> Self {
        let mut registers = [0; 256];
        for (register, value) in [
            (2, b'+'),
            (3, b'\r'),
            (4, b'\n'),
            (5, 8),
            (6, 2),
            (7, 50),
            (8, 2),
            (9, 6),
            (10, 14),
            (11, 95),
            (12, 50),
            (25, 5),
            (26, 1),
            (38, 20),
        ] {
            registers[register] = value;
        }
        Self {
            echo: true,
            quiet: false,
            verbose: true,
            result_set: 4,
            loudness: 2,
            monitor: 1,
            dcd: Dcd::AlwaysOn,
            registers,
        }
    }
}

impl Settings {
    /// Applies `command` if it only changes a setting, and tells whether it did.
    pub fn apply(&mut self, command: &Command) -> bool {
        match *command {
            Command::Echo(on) => self.echo = on,
            Command::Quiet(on) => self.quiet = on,
            Command::Verbose(on) => self.verbose = on,
            Command::ResultSet(level) => self.result_set = level,
            Command::Loudness(level) => self.loudness = level,
            Command::Monitor(mode) => self.monitor = mode,
            Command::CarrierDetect(follows) => {
                self.dcd = if follows {
                    Dcd::FollowsCarrier
                } else {
                    Dcd::AlwaysOn
                };
            }
            Command::SetRegister { register, value } => {
                self.registers[usize::from(register)] = value;
            }
            Command::Ignored => {}
            _ => return false,
        }
        true
    }

    #[must_use]
    pub fn register(&self, register: u8) -> u8 {
        self.registers[usize::from(register)]
    }

    #[must_use]
    pub fn auto_answer_rings(&self) -> u8 {
        self.registers[0]
    }

    #[must_use]
    pub fn escape(&self) -> Option<u8> {
        Some(self.registers[2]).filter(|&c| c <= 127)
    }

    #[must_use]
    pub fn terminator(&self) -> u8 {
        self.registers[3]
    }

    #[must_use]
    pub fn backspace(&self) -> u8 {
        self.registers[5]
    }

    /// The wait before dialling at a result level without dial tone detection.
    #[must_use]
    pub fn blind_dial_wait(&self) -> Duration {
        if matches!(self.result_set, 2 | 4) {
            Duration::ZERO
        } else {
            seconds(self.registers[6])
        }
    }

    #[must_use]
    pub fn carrier_wait(&self) -> Duration {
        seconds(self.registers[7])
    }

    #[must_use]
    pub fn comma_pause(&self) -> Duration {
        seconds(self.registers[8])
    }

    #[must_use]
    pub fn carrier_loss_hang_up(&self) -> Duration {
        Duration::from_millis(u64::from(self.registers[10]) * 100)
    }

    #[must_use]
    pub fn escape_guard(&self) -> Duration {
        Duration::from_millis(u64::from(self.registers[12]) * 20)
    }

    /// The bytes that report `code` to the computer, or nothing under `Q1`.
    #[must_use]
    pub fn report(&self, code: ResultCode) -> Vec<u8> {
        if self.quiet {
            return Vec::new();
        }
        let code = match code {
            ResultCode::Busy if self.result_set < 3 => ResultCode::NoCarrier,
            ResultCode::NoDialtone if !matches!(self.result_set, 2 | 4) => ResultCode::NoCarrier,
            code => code,
        };
        if self.verbose {
            self.line(code.words())
        } else {
            vec![b'0' + code.digit(), self.terminator()]
        }
    }

    /// Informational text, such as `ATI` output, framed like a verbal result.
    #[must_use]
    pub fn line(&self, text: &str) -> Vec<u8> {
        let end = [self.terminator(), self.registers[4]];
        [&end[..], text.as_bytes(), &end[..]].concat()
    }
}

fn seconds(value: u8) -> Duration {
    Duration::from_secs(u64::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_in_words_by_default() {
        assert_eq!(Settings::default().report(ResultCode::Ok), b"\r\nOK\r\n");
    }

    #[test]
    fn reports_digits_under_v0() {
        let settings = Settings {
            verbose: false,
            ..Settings::default()
        };
        assert_eq!(settings.report(ResultCode::NoCarrier), b"3\r");
        assert_eq!(settings.report(ResultCode::Busy), b"7\r");
    }

    #[test]
    fn reports_nothing_under_q1() {
        let settings = Settings {
            quiet: true,
            ..Settings::default()
        };
        assert!(settings.report(ResultCode::Connect).is_empty());
    }

    #[test]
    fn lower_result_levels_fold_codes_into_no_carrier() {
        let at = |result_set| Settings {
            result_set,
            ..Settings::default()
        };
        assert_eq!(at(0).report(ResultCode::Busy), b"\r\nNO CARRIER\r\n");
        assert_eq!(at(3).report(ResultCode::Busy), b"\r\nBUSY\r\n");
        assert_eq!(at(3).report(ResultCode::NoDialtone), b"\r\nNO CARRIER\r\n");
        assert_eq!(at(2).report(ResultCode::NoDialtone), b"\r\nNO DIALTONE\r\n");
    }

    #[test]
    fn follows_the_line_ending_registers() {
        let mut settings = Settings::default();
        settings.registers[3] = b'!';
        settings.registers[4] = b'~';
        assert_eq!(settings.report(ResultCode::Ok), b"!~OK!~");
    }

    #[test]
    fn times_come_from_the_registers() {
        let settings = Settings::default();
        assert_eq!(settings.carrier_wait(), Duration::from_secs(50));
        assert_eq!(settings.carrier_loss_hang_up(), Duration::from_millis(1400));
        assert_eq!(settings.escape_guard(), Duration::from_secs(1));
        assert_eq!(settings.blind_dial_wait(), Duration::ZERO);
        let blind = Settings {
            result_set: 3,
            ..Settings::default()
        };
        assert_eq!(blind.blind_dial_wait(), Duration::from_secs(2));
    }

    #[test]
    fn applies_only_commands_that_change_settings() {
        let mut settings = Settings::default();
        assert!(settings.apply(&Command::Echo(false)));
        assert!(settings.apply(&Command::SetRegister {
            register: 0,
            value: 1
        }));
        assert!(!settings.apply(&Command::Answer));
        assert!(!settings.echo);
        assert_eq!(settings.auto_answer_rings(), 1);
    }

    #[test]
    fn an_escape_character_above_127_disables_escape() {
        let mut settings = Settings::default();
        assert_eq!(settings.escape(), Some(b'+'));
        settings.registers[2] = 200;
        assert_eq!(settings.escape(), None);
    }
}
