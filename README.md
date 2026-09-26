# softmodem

A software modem that places real phone calls over SIP. A computer talks to
it through a serial port with AT commands, as it would to a modem from the
1990s. The modem turns the data into audio, and the call carries that audio
as G.711 A-law, like any other call.

## Speeds

| Recommendation | Rate |
|---|---|
| V.21 | 300 bit/s |
| V.22 | 1200 bit/s |
| V.22bis | 2400 bit/s |
| V.34 | up to 33 600 bit/s |
| V.90 | up to 56 000 bit/s down, 33 600 up |

Error control is V.42 (LAPM), with V.42bis compression. Automode starts with
V.8 and falls back to the best modulation both ends have. By default it
offers up to V.90.

## Using it

It builds with cargo: `cargo build --release`. On Linux the sound output
needs the ALSA headers and pkg-config. Nix builds it reliably with
everything it needs: `nix build`, or `nix develop` for a shell.

Two modems on one host, over a UDP "wire" instead of SIP:

```sh
softmodem wire --local 127.0.0.1:5300 --pty /tmp/isp --init ATS0=1
softmodem wire --local 127.0.0.1:5301 --peer 127.0.0.1:5300 --pty /tmp/caller
```

Then open a terminal program on `/tmp/caller` and type `ATDT0300`. `ATS0=1`
makes the other modem answer on the first ring.

Over SIP, registered with a PBX:

```sh
softmodem sip --registrar pbx.example.org --user 1001 --password-env SIP_PASSWORD --pty /tmp/modem
```

The password can also come from `--password` or `--password-file`. Without
any of the three, the modem asks for it on stdin.

The serial port can be a pseudoterminal (`--pty`), a TCP listener (`--tcp`),
a character device with DCD and RI through CUSE on Linux (`--cuse`), a tty
device the host has, such as a UART (`--serial`), or stdin and stdout. `--dump` records each call as WAV files, and `--speaker`
plays it on the sound output.

Common AT commands: `ATD` dials, `ATA` answers, `ATH` hangs up, `+++`
escapes to command mode, `ATO` goes back online, and `AT+MS=V34` or
`AT+MS=V90,0` picks the highest modulation, the second with automode off.
[docs/terminal.md](docs/terminal.md) lists all of them.

## What it cannot do yet

- The SIP path must carry A-law unchanged: no transcoding, no gain, no echo
  cancellation, no packet loss concealment. V.34 and slower cope with some
  of that, V.90 does not.
- A lost audio packet breaks a V.90 call, as V.90 retrains are not done
  yet. Nor are recovery timeouts.
- V.92, V.32bis, V.23 and Bell 103 are not implemented.
- It has been checked against spandsp, and against the Smart Link soft
  modem that [D-Modem](https://github.com/strozfriedberg/D-Modem) runs, in
  automated tests. It has not been checked against a hardware modem yet.

## AI disclosure

This project is written largely by an AI model, specifically Claude Opus
5.5. It is fairly tested, but expect bugs and weird behaviour, and do not
use it in production environments.

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).
