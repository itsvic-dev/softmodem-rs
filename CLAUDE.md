# softmodem

A V.21, V.22, V.22bis and V.34 modem with V.42 and V.42bis that places real calls
over SIP. The design is in `docs/`, starting at `docs/README.md`.

- `softmodem-dsp`: modulators, demodulators and tones over linear 8 kHz
  samples. No IO, no async, no dependencies on the other crates.
- `softmodem-link`: V.42 over a pump's bits: the detection phase, HDLC
  framing, LAPM, V.42bis, and the plain V.14 fallback. No IO, and time comes
  in as an argument.
- `softmodem-terminal`: what the computer sees. The pseudoterminal, the CUSE
  and TCP ports, the AT command parser, settings and S-registers, the line
  editor and the `+++` escape. No IO outside `pty`, `cuse` and `tcp`.
- `softmodem-transport`: carries audio frames for a call, and rings, answers
  and refuses calls. A-law, the reorder window, the UDP wire, an in-memory
  loopback, SIP through a registrar, WAV recording, and the speaker on the
  host sound output.
- `softmodem`: the modem state machine that joins them, and the binary.
- `softmodem-interop`: tests only, against spandsp's V.21, V.22, V.22bis,
  V.8, V.42, V.42bis and answer tones. It links spandsp through pkg-config, which only
  the dev shell provides. The default shell has spandsp 3.0.0, and
  `nix develop .#spandsp-3_1` has 3.1.1 with V.34
  (`nix/spandsp.nix`). Run the interop tests in both.
- `softmodem-slmodem`: tests only. The program that D-Modem's slmodemd runs
  on `ATD`, which dials a softmodem over the UDP wire. See
  `docs/interop.md`.

## Working here

- `nix develop --command <cmd>` for anything Rust. The system `cargo` is a
  nightly whose test binaries abort on the first panic.
- `nix/Cargo.nix` is generated, and a dependency change is not picked up by
  `nix build` until it is regenerated and committed:
  `nix develop --command crate2nix generate --output nix/Cargo.nix`.
- Modem tests use the loopback transport on paused tokio time
  (`start_paused`), so the answer sequence and ring timers cost nothing. Only
  `softmodem/tests/pty.rs` and `softmodem/tests/tcp.rs` run in real time.
- Two modems on one host, recording every call:
  `softmodem wire --local 127.0.0.1:5300 --pty /tmp/isp --init ATS0=1 --dump dumps`
  and `softmodem wire --local 127.0.0.1:5301 --peer 127.0.0.1:5300 --pty /tmp/caller --dump dumps`,
  then a terminal program on `/tmp/caller` and `ATDT0300`. Without `--pty`,
  `--cuse` or `--tcp` the serial port is stdin and stdout.
- `creds.txt` is ignored and holds live accounts for `pbx.vic.iw`. Never read
  or print it. Load it into the environment and pass variable names:
  `set -a; . ./creds.txt; set +a`, then
  `softmodem sip --registrar pbx.vic.iw --user "$USER1" --password-env USER1_PASS --pty /tmp/a`.
  The live tests run with
  `SOFTMODEM_PBX=pbx.vic.iw cargo test -p softmodem --test pbx -- --ignored --test-threads=1`.
- `pppd` needs root, so it is tested in a NixOS VM test, not by cargo:
  `nix build .#checks.aarch64-linux.ppp -L`. It needs a Linux builder with
  `kvm`, and it copies each side's WAV recordings into `result/`.
  `ppp` runs fixed V.21, `ppp-v22`, `ppp-v22bis` and `ppp-v90` fixed V.22,
  V.22bis and V.90, and `ppp-automode` the default, V.8 up to V.90.
- The project is GPL-3.0-or-later under REUSE: `REUSE.toml` covers every
  file, and `checks.<system>.reuse` runs `reuse lint`. A file under another
  licence needs its own annotation there and its text in `LICENSES/`.
- spandsp is LGPL-2.1: call it through `softmodem-interop` only, never read
  its source to shape our code. Its installed headers may be read to write
  the binding. The ITU texts in `docs/specs/` are the reference.
- The CUSE port is Linux only and needs root, so it has its own VM test,
  `checks.aarch64-linux.cuse`, which includes a nested QEMU guest under TCG
  and takes about 3 minutes.
- `checks.aarch64-linux.slmodemd-v22bis`, `slmodemd-v34` and
  `slmodemd-v90` call this modem from slmodemd in an x86-64 guest under
  TCG, and pass text both ways. In `slmodemd-v90` slmodemd is the V.90
  analogue modem, so it checks this modem's digital side, and
  `slmodemd-v90-renegotiate` adds noise toward slmodemd, which makes it
  renegotiate with silence. `result/dumps/softmodem/` has the call as one stereo WAV,
  slmodemd on the left. `slmodemd-v34-retrain` retrains from ATO1, and
  `slmodemd-v34-noise` adds noise that makes slmodemd retrain, and
  `slmodemd-v34-renegotiate` noise toward this modem only, which makes it
  renegotiate. Each check
  fails on its run's `status`, and the run itself, recording and journals
  included, is `.#checks.<system>.<check>.run`.
