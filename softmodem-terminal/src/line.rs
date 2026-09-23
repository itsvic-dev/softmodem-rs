//! Collects command lines from the computer in command mode.

use crate::settings::Settings;

const MAX_LINE: usize = 255;

/// What one byte from the computer completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The text after `AT`, when the terminator arrives.
    Line(Vec<u8>),
    /// `A/`: run the previous line again.
    Repeat(Vec<u8>),
}

#[derive(Debug, Default)]
pub struct LineEditor {
    buffer: Vec<u8>,
    last: Vec<u8>,
}

impl LineEditor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one byte, appends what should be echoed to `echo`, and returns
    /// a line once one is complete. Bytes before an `A` are ignored, as are
    /// lines that do not start with `AT`.
    pub fn push(&mut self, byte: u8, settings: &Settings, echo: &mut Vec<u8>) -> Option<Input> {
        if byte == settings.terminator() {
            if settings.echo {
                echo.push(byte);
            }
            let line = std::mem::take(&mut self.buffer);
            let text = line.get(2..).filter(|_| starts_with(&line, b"AT"))?;
            self.last = text.to_vec();
            return Some(Input::Line(text.to_vec()));
        }
        if byte == settings.backspace() {
            if self.buffer.pop().is_some() && settings.echo {
                echo.extend([byte, b' ', byte]);
            }
            return None;
        }
        if self.buffer.is_empty() && !byte.eq_ignore_ascii_case(&b'A') {
            return None;
        }
        if self.buffer.len() >= MAX_LINE || byte == b'\n' {
            return None;
        }
        if settings.echo {
            echo.push(byte);
        }
        self.buffer.push(byte);
        if self.buffer.len() == 2 {
            if byte == b'/' {
                self.buffer.clear();
                return Some(Input::Repeat(self.last.clone()));
            }
            if !byte.eq_ignore_ascii_case(&b'T') {
                self.buffer.clear();
            }
        }
        None
    }
}

fn starts_with(line: &[u8], prefix: &[u8]) -> bool {
    line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(editor: &mut LineEditor, settings: &Settings, bytes: &[u8]) -> (Vec<Input>, Vec<u8>) {
        let mut echo = Vec::new();
        let inputs = bytes
            .iter()
            .filter_map(|&b| editor.push(b, settings, &mut echo))
            .collect();
        (inputs, echo)
    }

    #[test]
    fn yields_the_text_after_at_and_echoes_it() {
        let settings = Settings::default();
        let (inputs, echo) = feed(&mut LineEditor::new(), &settings, b"atdt0300\r");
        assert_eq!(inputs, [Input::Line(b"dt0300".to_vec())]);
        assert_eq!(echo, b"atdt0300\r");
    }

    #[test]
    fn does_not_echo_under_e0() {
        let settings = Settings {
            echo: false,
            ..Settings::default()
        };
        let (inputs, echo) = feed(&mut LineEditor::new(), &settings, b"AT\r");
        assert_eq!(inputs, [Input::Line(Vec::new())]);
        assert!(echo.is_empty());
    }

    #[test]
    fn ignores_noise_before_the_prefix() {
        let settings = Settings::default();
        let (inputs, _) = feed(&mut LineEditor::new(), &settings, b"\n~xAqATZ\r");
        assert_eq!(inputs, [Input::Line(b"Z".to_vec())]);
    }

    #[test]
    fn a_line_without_the_prefix_is_dropped() {
        let settings = Settings::default();
        let (inputs, _) = feed(&mut LineEditor::new(), &settings, b"A\r");
        assert!(inputs.is_empty());
    }

    #[test]
    fn backspace_erases_and_echoes_the_erasure() {
        let settings = Settings::default();
        let (inputs, echo) = feed(&mut LineEditor::new(), &settings, b"ATX\x084\r");
        assert_eq!(inputs, [Input::Line(b"4".to_vec())]);
        assert_eq!(echo, b"ATX\x08 \x084\r");
    }

    #[test]
    fn a_slash_repeats_the_previous_line_at_once() {
        let settings = Settings::default();
        let mut editor = LineEditor::new();
        feed(&mut editor, &settings, b"ATDT0300\r");
        let (inputs, _) = feed(&mut editor, &settings, b"A/");
        assert_eq!(inputs, [Input::Repeat(b"DT0300".to_vec())]);
    }

    #[test]
    fn follows_the_terminator_register() {
        let mut settings = Settings::default();
        settings.registers[3] = b'!';
        let (inputs, _) = feed(&mut LineEditor::new(), &settings, b"ATH\r!");
        assert_eq!(inputs, [Input::Line(b"H\r".to_vec())]);
    }
}
