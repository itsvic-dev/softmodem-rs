# softmodem

A V.21 modem that places real calls over SIP. The design is `docs/dialup.md`.

- `softmodem-dsp`: modulators, demodulators and tones over linear 8 kHz
  samples. No IO, no async, no dependencies on the other crates.
- `softmodem-terminal`: what the computer sees. The pseudoterminal, the AT
  command parser, settings and S-registers, the line editor and the `+++`
  escape. No IO outside `pty`.
- `softmodem-transport`: carries audio frames for a call, and rings, answers
  and refuses calls. A-law, the reorder window, the UDP wire, an in-memory
  loopback and WAV recording.
- `softmodem`: the modem state machine that joins them, and the binary.

## Working here

- `nix develop --command <cmd>` for anything Rust. The system `cargo` is a
  nightly whose test binaries abort on the first panic.
- `nix/Cargo.nix` is generated, and a dependency change is not picked up by
  `nix build` until it is regenerated and committed:
  `nix develop --command crate2nix generate --output nix/Cargo.nix`.
- Modem tests use the loopback transport on paused tokio time
  (`start_paused`), so the answer sequence and ring timers cost nothing. Only
  `softmodem/tests/pty.rs` runs in real time.
- Two modems on one host, recording every call:
  `softmodem wire --local 127.0.0.1:5300 --pty /tmp/isp --init ATS0=1 --dump dumps`
  and `softmodem wire --local 127.0.0.1:5301 --peer 127.0.0.1:5300 --pty /tmp/caller --dump dumps`,
  then a terminal program on `/tmp/caller` and `ATDT0300`. Without `--pty` the
  serial port is stdin and stdout.
- `pppd` needs root, so it is tested in a NixOS VM test, not by cargo:
  `nix build .#checks.aarch64-linux.ppp -L`. It needs a Linux builder with
  `kvm`, and it copies each side's WAV recordings into `result/`.
- The CUSE port is Linux only and needs root, so it has its own VM test,
  `checks.aarch64-linux.cuse`, which includes a nested QEMU guest under TCG
  and takes about 3 minutes.
