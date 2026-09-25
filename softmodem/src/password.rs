// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::{self, Write};
use std::os::fd::AsFd;
use std::path::PathBuf;

use anyhow::Context;
use clap::Args;
use rustix::io::Errno;
use rustix::termios::{self, LocalModes, OptionalActions};

#[derive(Args)]
#[group(multiple = false)]
pub struct Source {
    /// The password, which other users of the host can see in the process list.
    #[arg(long)]
    password: Option<String>,
    /// Environment variable that holds the password.
    #[arg(long, value_name = "VARIABLE")]
    password_env: Option<String>,
    /// File whose first line is the password.
    #[arg(long, value_name = "PATH")]
    password_file: Option<PathBuf>,
}

impl Source {
    pub fn read(self) -> anyhow::Result<String> {
        if let Some(password) = self.password {
            Ok(password)
        } else if let Some(variable) = self.password_env {
            std::env::var(&variable)
                .with_context(|| format!("reading the password from ${variable}"))
        } else if let Some(path) = self.password_file {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading the password from {}", path.display()))?;
            Ok(text.lines().next().unwrap_or_default().to_owned())
        } else {
            prompt().context("reading the password from stdin")
        }
    }
}

fn prompt() -> io::Result<String> {
    let stdin = io::stdin();
    let terminal = termios::tcgetattr(stdin.as_fd()).ok();
    if let Some(original) = &terminal {
        eprint!("Password: ");
        io::stderr().flush()?;
        let mut hidden = original.clone();
        hidden.local_modes.remove(LocalModes::ECHO);
        termios::tcsetattr(stdin.as_fd(), OptionalActions::Flush, &hidden)?;
    }
    let line = read_line(&stdin);
    if let Some(original) = &terminal {
        termios::tcsetattr(stdin.as_fd(), OptionalActions::Now, original)?;
        eprintln!();
    }
    line
}

// A byte at a time, so that nothing after the line is taken from a serial port on stdin.
fn read_line(stdin: &io::Stdin) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0];
    loop {
        match rustix::io::read(stdin.as_fd(), &mut byte) {
            Ok(0) if line.is_empty() => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
            Err(Errno::INTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Source;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        password: Source,
    }

    fn parse(args: &[&str]) -> Result<Source, clap::Error> {
        Cli::try_parse_from(std::iter::once("softmodem").chain(args.iter().copied()))
            .map(|cli| cli.password)
    }

    #[test]
    fn takes_the_password_from_the_command_line() {
        let password = parse(&["--password", "secret"]).unwrap();
        assert_eq!(password.read().unwrap(), "secret");
    }

    #[test]
    fn takes_the_first_line_of_a_file() {
        let path = std::env::temp_dir().join(format!("softmodem-password-{}", std::process::id()));
        std::fs::write(&path, "secret\nnot this\n").unwrap();
        let password = parse(&["--password-file", path.to_str().unwrap()]).unwrap();
        let read = password.read();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(read.unwrap(), "secret");
    }

    #[test]
    fn refuses_two_sources() {
        assert!(
            parse(&["--password", "secret", "--password-env", "PASSWORD"]).is_err(),
            "one source would silently win over the other"
        );
    }
}
