# softmodem

A V.21 modem that places real calls over SIP. The design is `docs/dialup.md`.

- `softmodem-dsp`: modulators and demodulators over linear 8 kHz samples. No
  IO, no async, no dependencies on the other crates.
- `softmodem`: the binary.

## Working here

- `nix develop --command <cmd>` for anything Rust.
- `nix/Cargo.nix` is generated, and a dependency change is not picked up by
  `nix build` until it is regenerated and committed:
  `nix develop --command crate2nix generate --output nix/Cargo.nix`.
