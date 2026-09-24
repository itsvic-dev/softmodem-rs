//! What the AT commands set, and how the modem answers the computer.

use std::time::Duration;

use crate::command::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    Ok,
    /// A connection at 300 bit/s.
    Connect,
    Connect1200,
    Connect2400,
    /// A connection above 2400 bit/s, which V.250 gives no digit of its own.
    ConnectAt(u32),
    Ring,
    NoCarrier,
    Error,
    NoDialtone,
    Busy,
}

impl ResultCode {
    /// The `CONNECT` code for a connection at `bit_rate`.
    #[must_use]
    pub fn connect(bit_rate: u32) -> Self {
        match bit_rate {
            1200 => Self::Connect1200,
            2400 => Self::Connect2400,
            rate if rate > 2400 => Self::ConnectAt(rate),
            _ => Self::Connect,
        }
    }

    fn digit(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Connect | Self::ConnectAt(_) => 1,
            Self::Ring => 2,
            Self::NoCarrier => 3,
            Self::Error => 4,
            Self::Connect1200 => 5,
            Self::NoDialtone => 6,
            Self::Busy => 7,
            Self::Connect2400 => 10,
        }
    }

    fn words(self) -> String {
        match self {
            Self::Ok => "OK".into(),
            Self::Connect => "CONNECT".into(),
            Self::Connect1200 => "CONNECT 1200".into(),
            Self::Connect2400 => "CONNECT 2400".into(),
            Self::ConnectAt(rate) => format!("CONNECT {rate}"),
            Self::Ring => "RING".into(),
            Self::NoCarrier => "NO CARRIER".into(),
            Self::Error => "ERROR".into(),
            Self::NoDialtone => "NO DIALTONE".into(),
            Self::Busy => "BUSY".into(),
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
    pub modulation: Modulation,
    pub error_control: ErrorControl,
    pub compression: Compression,
    pub registers: [u8; 256],
}

/// What `+ES` sets, how to try V.42, with V.250's subparameters, and what
/// `+ER` sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorControl {
    pub orig_rqst: u8,
    pub orig_fbk: u8,
    pub ans_fbk: u8,
    /// Whether to report the error control in use before CONNECT.
    pub report: bool,
}

/// What `+DS` sets, how to ask for V.42 bis, and what `+DR` sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compression {
    /// 0 none, 1 transmit only, 2 receive only, 3 both, from the DTE's side.
    pub direction: u8,
    /// Whether to hang up unless the far end agrees to `direction`.
    pub required: bool,
    /// The most codewords to agree to, V.42 bis P1.
    pub max_dict: u16,
    /// The longest string to agree to, V.42 bis P2.
    pub max_string: u8,
    /// Whether to report the compression in use before CONNECT.
    pub report: bool,
}

impl Default for Compression {
    fn default() -> Self {
        Self {
            direction: 3,
            required: false,
            max_dict: 2048,
            max_string: 32,
            report: false,
        }
    }
}

impl Compression {
    #[must_use]
    pub fn transmit(&self) -> bool {
        self.direction & 1 != 0
    }

    #[must_use]
    pub fn receive(&self) -> bool {
        self.direction & 2 != 0
    }
}

impl Default for ErrorControl {
    fn default() -> Self {
        Self {
            orig_rqst: 3,
            orig_fbk: 0,
            ans_fbk: 2,
            report: false,
        }
    }
}

impl ErrorControl {
    /// Whether an originating modem tries V.42.
    #[must_use]
    pub fn originator_tries(&self) -> bool {
        self.orig_rqst >= 2
    }

    /// Whether an originating modem starts with the detection phase.
    #[must_use]
    pub fn originator_detects(&self) -> bool {
        self.orig_rqst == 3
    }

    /// Whether an originating modem hangs up without V.42.
    #[must_use]
    pub fn originator_requires(&self) -> bool {
        self.orig_fbk >= 2
    }

    /// Whether an answering modem tries V.42.
    #[must_use]
    pub fn answerer_tries(&self) -> bool {
        self.ans_fbk >= 2
    }

    /// Whether an answering modem hangs up without V.42.
    #[must_use]
    pub fn answerer_requires(&self) -> bool {
        self.ans_fbk >= 4
    }
}

/// What `+MS` sets: the highest modulation to use, and whether automode may
/// fall back from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modulation {
    pub carrier: Carrier,
    pub automode: bool,
}

impl Default for Modulation {
    fn default() -> Self {
        Self {
            carrier: Carrier::V34,
            automode: true,
        }
    }
}

/// A modulation, with the names V.250 gives them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    V21,
    V22,
    V22bis,
    V34,
    V90,
}

impl Carrier {
    pub const ALL: [Self; 5] = [Self::V21, Self::V22, Self::V22bis, Self::V34, Self::V90];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::V21 => "V21",
            Self::V22 => "V22",
            Self::V22bis => "V22B",
            Self::V34 => "V34",
            Self::V90 => "V90",
        }
    }
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
            modulation: Modulation::default(),
            error_control: ErrorControl::default(),
            compression: Compression::default(),
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
            Command::SetCarrier { carrier, automode } => {
                self.modulation = Modulation { carrier, automode };
            }
            Command::SetErrorControl {
                orig_rqst,
                orig_fbk,
                ans_fbk,
            } => {
                let current = &mut self.error_control;
                current.orig_rqst = orig_rqst.unwrap_or(current.orig_rqst);
                current.orig_fbk = orig_fbk.unwrap_or(current.orig_fbk);
                current.ans_fbk = ans_fbk.unwrap_or(current.ans_fbk);
            }
            Command::ReportErrorControl(on) => self.error_control.report = on,
            Command::SetCompression {
                direction,
                required,
                max_dict,
                max_string,
            } => {
                let current = &mut self.compression;
                current.direction = direction.unwrap_or(current.direction);
                current.required = required.unwrap_or(current.required);
                current.max_dict = max_dict.unwrap_or(current.max_dict);
                current.max_string = max_string.unwrap_or(current.max_string);
            }
            Command::ReportCompression(on) => self.compression.report = on,
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

    /// The speaker volume under `L` and `M`, from 0 for off to 1 for full.
    #[must_use]
    pub fn speaker_gain(&self, connected: bool) -> f32 {
        let on = match self.monitor {
            0 => false,
            1 => !connected,
            _ => true,
        };
        match (on, self.loudness) {
            (false, _) => 0.0,
            (true, 0 | 1) => 0.25,
            (true, 2) => 0.5,
            (true, _) => 1.0,
        }
    }

    /// The bytes that report `code` to the computer, or nothing under `Q1`.
    #[must_use]
    pub fn report(&self, code: ResultCode) -> Vec<u8> {
        if self.quiet {
            return Vec::new();
        }
        let code = match code {
            ResultCode::Connect1200 | ResultCode::Connect2400 | ResultCode::ConnectAt(_)
                if self.result_set == 0 =>
            {
                ResultCode::Connect
            }
            ResultCode::Busy if self.result_set < 3 => ResultCode::NoCarrier,
            ResultCode::NoDialtone if !matches!(self.result_set, 2 | 4) => ResultCode::NoCarrier,
            code => code,
        };
        if self.verbose {
            self.line(&code.words())
        } else {
            let mut bytes = code.digit().to_string().into_bytes();
            bytes.push(self.terminator());
            bytes
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
    fn reports_the_rate_above_300_bit_s_except_under_x0() {
        let at = |result_set, verbose| Settings {
            verbose,
            result_set,
            ..Settings::default()
        };
        let connect = ResultCode::connect(1200);
        assert_eq!(at(1, true).report(connect), b"\r\nCONNECT 1200\r\n");
        assert_eq!(at(4, false).report(connect), b"5\r");
        assert_eq!(at(0, true).report(connect), b"\r\nCONNECT\r\n");
        assert_eq!(
            at(4, true).report(ResultCode::connect(300)),
            b"\r\nCONNECT\r\n"
        );
        let connect = ResultCode::connect(2400);
        assert_eq!(at(4, true).report(connect), b"\r\nCONNECT 2400\r\n");
        assert_eq!(at(4, false).report(connect), b"10\r");
        assert_eq!(at(0, true).report(connect), b"\r\nCONNECT\r\n");
        let connect = ResultCode::connect(33_600);
        assert_eq!(at(4, true).report(connect), b"\r\nCONNECT 33600\r\n");
        assert_eq!(at(4, false).report(connect), b"1\r");
        assert_eq!(at(0, true).report(connect), b"\r\nCONNECT\r\n");
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
        assert!(settings.apply(&Command::SetCarrier {
            carrier: Carrier::V22,
            automode: false
        }));
        assert!(!settings.apply(&Command::Answer));
        assert!(!settings.apply(&Command::ReadCarrier));
        assert!(!settings.echo);
        assert_eq!(settings.auto_answer_rings(), 1);
        assert_eq!(
            settings.modulation,
            Modulation {
                carrier: Carrier::V22,
                automode: false
            }
        );
    }

    #[test]
    fn offers_v34_with_automode_by_default() {
        assert_eq!(
            Settings::default().modulation,
            Modulation {
                carrier: Carrier::V34,
                automode: true
            }
        );
    }

    #[test]
    fn tries_v42_and_falls_back_by_default() {
        let control = Settings::default().error_control;
        assert!(control.originator_tries() && control.originator_detects());
        assert!(control.answerer_tries());
        assert!(!control.originator_requires() && !control.answerer_requires());
    }

    #[test]
    fn offers_v42bis_both_ways_by_default_and_ds_changes_what_it_is_given() {
        let mut settings = Settings::default();
        let compression = settings.compression;
        assert!(compression.transmit() && compression.receive() && !compression.required);
        assert!(settings.apply(&Command::SetCompression {
            direction: Some(2),
            required: None,
            max_dict: Some(4096),
            max_string: None,
        }));
        assert_eq!(
            settings.compression,
            Compression {
                direction: 2,
                max_dict: 4096,
                ..Compression::default()
            }
        );
        assert!(!settings.compression.transmit() && settings.compression.receive());
    }

    #[test]
    fn es_changes_only_the_subparameters_given() {
        let mut settings = Settings::default();
        assert!(settings.apply(&Command::SetErrorControl {
            orig_rqst: None,
            orig_fbk: Some(3),
            ans_fbk: None,
        }));
        assert_eq!(
            settings.error_control,
            ErrorControl {
                orig_fbk: 3,
                ..ErrorControl::default()
            }
        );
        assert!(settings.error_control.originator_requires());
    }

    #[test]
    #[expect(clippy::float_cmp, reason = "the gains are exact constants")]
    fn the_speaker_follows_m_and_l() {
        let with = |monitor, loudness| Settings {
            loudness,
            monitor,
            ..Settings::default()
        };
        assert_eq!(with(0, 3).speaker_gain(false), 0.0);
        assert_eq!(with(1, 2).speaker_gain(false), 0.5);
        assert_eq!(with(1, 2).speaker_gain(true), 0.0);
        assert_eq!(with(2, 3).speaker_gain(true), 1.0);
        assert_eq!(with(2, 0).speaker_gain(true), 0.25);
    }

    #[test]
    fn an_escape_character_above_127_disables_escape() {
        let mut settings = Settings::default();
        assert_eq!(settings.escape(), Some(b'+'));
        settings.registers[2] = 200;
        assert_eq!(settings.escape(), None);
    }
}
