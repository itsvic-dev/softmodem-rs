# softmodem

A V.21 modem that places real calls over SIP. The design is `docs/dialup.md`.

- `softmodem-dsp`: modulators and demodulators over linear 8 kHz samples. No
  IO, no async, no dependencies on the other crates.
- `softmodem-transport`: carries audio frames for a call. A-law, the reorder
  window, the UDP wire and WAV recording.
- `softmodem`: the data pump that runs the modem over a call, and the binary.

## Working here

- `nix develop --command <cmd>` for anything Rust. The system `cargo` is a
  nightly whose test binaries abort on the first panic.
- `nix/Cargo.nix` is generated, and a dependency change is not picked up by
  `nix build` until it is regenerated and committed:
  `nix develop --command crate2nix generate --output nix/Cargo.nix`.
- Two instances on one host, recording every call:
  `softmodem wire answer --local 127.0.0.1:5300 --dump dumps` and
  `softmodem wire originate --peer 127.0.0.1:5300 --dump dumps < file`.
  The answering side hangs up when its stdin ends, so keep it open.
