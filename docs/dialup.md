# softmodem

**Status:** draft, nothing built yet. This records a design and the reasoning
behind it, including the options that were rejected, so that the rejected ones
do not have to be investigated twice.

This document covers the `softmodem` program only. The network side (Asterisk
endpoint, dial plan, PPP address pool, the ATA and the retro client) lives in
`vic-nix-config` and is out of scope here, apart from the interfaces this
program has to meet.

## Goal

A modem places an ordinary SIP call, a PPP session comes up over the
modulated audio, and the caller gets an address from the ISP.

The point is not bandwidth. The point is that the call is a real call, the
carrier is a real carrier, and a machine from 1998 can dial in the way it
would have dialled the internet.

Non-goals:

- Speed. The first target is 300 bit/s.
- 56k. See "Why not 56k" below.
- V.42bis compression, for now. It is the next milestone.

## What the channel gives, and what it must not do

The audio path is A-law RTP from end to end, with the PBX in the media path.
A modem needs on top of that:

- **G.711 only, no transcoding.** An Opus leg anywhere destroys the waveform.
  `softmodem` offers PCMA and nothing else.
- **No voice processing.** No VAD, no silence suppression, no comfort noise,
  no echo cancellation. All of these are designed to discard exactly the
  signal a modem is sending.
- **No adaptive jitter buffer.** A buffer that resizes mid-call inserts or
  drops samples, and that breaks bit timing. See "Receive path" below.
- **Loss matters, latency does not.** At 300 bit/s a modem is indifferent to
  half a second of delay and intolerant of a dropped packet. This is the
  opposite of the tuning a PBX ships with.

## Speed, and the ladder

Modulations, in order of how hard they are to implement:

| Standard | Rate | Nature | Verdict |
|---|---|---|---|
| V.21, Bell 103 | 300 bit/s | FSK, full duplex | first target |
| V.23 | 1200/75 | FSK, asymmetric | possible, see below |
| V.22 | 1200 bit/s | DPSK, full duplex | the real second step |
| V.22bis | 2400 bit/s | QAM, adaptive equaliser | a different project |
| V.32bis | 14.4k | QAM, echo cancellation | no |
| V.34 | 33.6k | QAM, line probing | no |
| V.90 | 56k down | PCM codepoints | see below |

The cliff is between FSK and everything after it, and it is not about bit
rate. FSK is two tones and a slicer. PSK and QAM need carrier recovery and
timing recovery, and QAM adds an adaptive equaliser. That is where a homegrown
modem stops being a weekend and starts being a season.

V.23 is FSK and so is cheap, but the 75 bit/s back channel makes PPP painful,
and many modems outside Europe do not implement it. V.22 is harder but is
supported by nearly every modem ever built, so it is the more useful second
step.

### Why not 56k

V.90 works by having one end sit digitally on the network and inject PCM
codepoints directly, with exactly one D/A conversion on the path. An A-law
RTP stream has *no* analogue segment at all. Without loss, the channel is a
64 kbit/s digital pipe, and a program at each end could send raw bytes as
samples. That is excluded by the goal: it is not a carrier, and no real modem
could take part.

With a real modem at one end, the problem is that no maintainable
implementation exists at either end.

- The analogue client side of V.90 exists in exactly one place in the free
  world: `dsplibs.o`, a 1.2 MB proprietary Smart Link binary vendored into
  `slmodem` and from there into D-Modem. It is 32-bit x86 only, cannot be
  fixed, and cannot be improved.
- The digital server side exists only in `cryan209/v90modem`, a young
  single-author research tree with no licence file, a patched Conexant modem
  ROM, and 374 MB of vendored spandsp and PJSIP. Real work, genuinely
  interesting, not a dependency.

So 56k reduces to "get two things we cannot maintain to talk to each other".
It stays on the shelf as a curiosity, not a plan.

## Architecture

```
caller ── pty ── softmodem ── SIP/RTP alaw ──> PBX ──> softmodem (answer) ── pty ── pppd
```

One program, `softmodem`, in both roles. It is a SIP user agent in its own
right, not an Asterisk module and not an AudioSocket client. Thus the calling
side is a normal endpoint that can live anywhere, and it can later be replaced
by a real modem behind an ATA without a change to the answering side.

Three layers, deliberately separable: transport, modem, terminal.

### SIP and RTP

The `ezk` crate family covers this in pure Rust: `ezk-sip-ua` (0.9.1, April
2026) with `ezk-rtp` (0.2.1, July 2026), plus the SIP types, SDP and auth
crates alongside them. `rvoip` (0.3.10) is a newer alternative with far less
use, and `rsip` is parse-only and stale since 2022. A-law is a lookup table,
so there is no codec dependency.

### Modem

A stateful streaming processor over linear samples: A-law is decoded to `i16`
at the RTP boundary and never seen by the DSP. The modulator and demodulator
each hold their state across buffers (filter history, oscillator phase, bit
timing) and expose one method, roughly `process(&mut self, input) -> output`.
No IO, no SIP, no async, no globals.

The modulator is a phase-continuous NCO switching between two frequencies. At
8000 samples/s and 300 bit/s, one bit is 26.67 samples, so bit boundaries are
tracked with a fractional accumulator, not an integer sample count. The
demodulator is a bandpass, a discriminator or a pair of correlators, a slicer,
and bit timing recovery.

Each modulation is a data pump: its own handshake, modulator and demodulator
behind one trait, in its own file, chosen for each call by `+MS`. The line
around it holds only what all of them share: the V.25 answer sequence, the
link over the pump's bits, and the bytes that arrive before `CONNECT`. The
link, in `softmodem-link`, is V.42 or plain start-stop characters. See
"V.42" below.

The V.22 modulator shapes each symbol with a square root raised cosine over
seven symbols. The demodulator mixes the channel down to baseband, applies
the same filter, recovers symbol timing with a Gardner detector, and decodes
each symbol by its phase change from the one before. That needs no carrier
recovery, as ±7 Hz turns the phase by only 4° a symbol.

V.22bis shares that transmitter and front end. At 2400 bit/s the point inside
a quadrant is absolute, so its receiver adds an AGC, a 17-tap equaliser at
half-symbol spacing adapted by normalised LMS, and a second-order phase
locked loop, both driven by decisions. They train on the scrambled ones at
1200 bit/s that follow S1. The handshake signals themselves, S1 during data,
and the V.22 fallback go through the differential V.22 demodulator running
beside it: a decision-directed equaliser learns to flatten S1, which repeats
every two symbols, and so would erase it.

### Terminal

`openpty`, 8N1 async framing, and an AT interpreter.

The computer talks to the modem, never to the line, in command mode. It
controls the modem with the basic Hayes command set, and the modem places and
takes calls through its transport as a result. One process both dials and
answers, as one modem on one line does.

#### Modes

- **Command mode, on hook.** Bytes from the computer are command lines.
  An incoming call gives `RING`, repeated every 6 s while the far end waits.
- **Dialling and handshake.** After `ATD` or `ATA`. Any byte from the
  computer aborts with `NO CARRIER`, and so does no `CONNECT` within `S7`.
- **Data mode.** Bytes pass to and from the line. `+++` with guard times
  goes to online command mode.
- **Online command mode.** The call stays up, the line idles on mark. `ATO`
  goes back to data mode, `ATH` hangs up.

The far end hanging up, or carrier lost for `S10`, gives `NO CARRIER` and
command mode, on hook.

#### Commands

| Command | Effect here |
|---|---|
| `A` | Answer a ringing call. |
| `A/` | Repeat the last command line, at once, without `AT` or CR. |
| `D` | Dial. See the modifiers below. |
| `E0`, `E1` | Command echo off, on. |
| `H0`, `H1` | On hook (hang up), off hook (incoming calls get busy). |
| `I0` to `I9` | Identification. `I0` product, `I3` version, `I4` modulations. |
| `L0` to `L3` | Speaker volume. `L0` and `L1` low, `L2` medium, `L3` high. |
| `M0` to `M2` | Speaker off, on until `CONNECT`, always on. |
| `O`, `O1` | Return to data mode from online command mode. `O1` retrains a V.22bis line first. |
| `Q0`, `Q1` | Result codes shown, suppressed. |
| `V0`, `V1` | Result codes as digits, as words. |
| `X0` to `X4` | Which result codes are used, see below. |
| `Z` | Hang up and reset to the stored profile. |
| `Sn=v`, `Sn?` | Write, read an S-register. |
| `+MS=<carrier>[,<automode>]` | The highest modulation for the next call, `V21`, `V22` or `V22B`, and whether automode may fall back from it. The default is `V22B` with automode. |
| `+MS?`, `+MS=?` | Read the modulation, list those supported. |
| `+ES=<orig_rqst>[,<orig_fbk>[,<ans_fbk>]]` | How to try V.42, as V.250 § 6.5.1. The default is `3,0,2`: try it with the detection phase, and fall back to plain data. |
| `+ES?`, `+ES=?` | Read the error control, list the values supported. |
| `+ER=0`, `+ER=1` | Report the error control in use before `CONNECT`, off, on. |
| `\N0` to `\N3` | Set all of `+ES`: `\N0` and `\N1` no V.42 (`1,0,1`), `\N2` V.42 required (`3,3,5`), `\N3` V.42 if the far end has it (`3,0,2`). |

`+ES` takes these values:

- `<orig_rqst>`, when the modem dials: `0` or `1` no V.42, `2` V.42 without
  the detection phase, `3` V.42 with it. `4`, the deleted alternative
  protocol, gives `ERROR`.
- `<orig_fbk>`, when the modem dials: `0` or `1` fall back to plain data,
  `2` or `3` hang up without V.42.
- `<ans_fbk>`, when the modem answers: `0` or `1` no V.42, `2` or `3` V.42
  if the far end has it, `4` or `5` hang up without V.42.

V.250 tells direct and buffered operation apart. On a pseudoterminal there is
no DTE rate to match, so they are the same here. A subparameter left out
keeps its value.

`+MS` takes the V.250 form `+MS=<carrier>[,<automode>[,<rates>...]]`. The
rates are accepted and ignored. As V.250 § 6.4.2 has it, `+MS=<carrier>` on
its own turns automode on again, so a fixed modulation needs `,0`:
`AT+MS=V21,0`. Without automode, V.22bis still falls back to V.22, which is
part of V.22bis itself. As V.250 requires, a basic command after `+MS` on the
same line needs a `;` first: `AT+MS=V22,0;E0`.

Other extended commands (`&`, `\`, `%` and `+` prefixes) are accepted and
return `OK` without effect, because chat scripts send chipset-specific
strings such as `AT&C1&D2` and fail on `ERROR`.

The stored profile is the Hayes defaults with the `--init` command line
applied, for example `--init 'ATS0=1'` on the ISP. It is applied at start
and again by every `ATZ`. So the ISP's `pppd` needs no chat script.

#### Dial modifiers

A VoIP call sends the whole number at once, so most modifiers have no
meaning. `T` and `P` are ignored. `W` and `@` return at once, as there is
always a dial tone and never a wait for quiet. `!` is ignored. `,` waits `S8`
seconds before the call is placed. These work as on a real modem:

- `R` dials in reverse mode: the modem answers the call it placed, with
  answer tone and channel 2.
- `;` places the call and returns to command mode without a handshake.
- `L` redials the last number.

What remains is digits, `*`, `#` and `A` to `D`, which become the number
given to the transport. For SIP that becomes a URI under dialling rules that
are configuration, not code.

#### Result codes

| Digit | Words | From |
|---|---|---|
| 0 | `OK` | `X0` |
| 1 | `CONNECT` | `X0` |
| 2 | `RING` | `X0` |
| 3 | `NO CARRIER` | `X0` |
| 4 | `ERROR` | `X0` |
| 5 | `CONNECT 1200` | `X1` |
| 6 | `NO DIALTONE` | `X2`, `X4` |
| 7 | `BUSY` | `X3`, `X4` |
| 10 | `CONNECT 2400` | `X1` |

At 300 bit/s Hayes reports plain `CONNECT`. Under `X0`, `CONNECT 1200` and
`CONNECT 2400` are plain `CONNECT` too. Below the level that has them, `BUSY` and `NO DIALTONE` become `NO CARRIER`.
The default is `X4`. `NO DIALTONE` never happens. With `V1` each code is
framed by CR LF, with `V0` it is the number and CR, both using `S3` and `S4`.

Under `+ER=1`, `+ER: LAPM` or `+ER: NONE` comes on its own line before
`CONNECT`, in words under `V0` too, as V.250 § 6.5.5 has it. `CONNECT`
itself does not change with V.42, so chat scripts that wait for
`CONNECT 2400` still work.

#### S-registers

All 256 hold a value. These have an effect:

| Register | Meaning | Default |
|---|---|---|
| `S0` | Rings before auto-answer, 0 for never. | 0 |
| `S1` | Rings counted so far. | 0 |
| `S2` | Escape character. | 43, `+` |
| `S3` | Line terminator. | 13, CR |
| `S4` | Response line feed. | 10, LF |
| `S5` | Backspace. | 8 |
| `S6` | Seconds to wait before a blind dial, with `X0`, `X1`, `X3`. | 2 |
| `S7` | Seconds to wait for `CONNECT` after `D` or `A`. | 50 |
| `S8` | Seconds per `,` in a dial string. | 2 |
| `S10` | Tenths of a second without carrier before hanging up. | 14 |
| `S12` | Escape guard time, in fiftieths of a second. | 50 |

`S9`, carrier detect time, is stored but not used: V.21 sets it to 300 to
700 ms, and its demodulator keeps 400 ms. V.22 sets 105 to 205 ms, and its
demodulator keeps 150 ms.

#### Answer sequence

The answering modem sends silence for 2 s, the 2100 Hz answer tone for
3.3 s, 75 ms of silence, then channel 2 mark, and reports `CONNECT` when it
detects channel 1. The originating modem is silent until the answer tone has
ended and it detects channel 2, then sends channel 1 mark and reports
`CONNECT`. It ignores carrier while the answer tone lasts, because the tone
is close enough to channel 2 to trip carrier detect.

Under V.22 the answer tone and the gap are the same, and the V.22 handshake
follows, as described under V.22 below. Each end reports `CONNECT 1200`
765 ms after it hears the other's scrambled ones. Under V.22bis each end
reports `CONNECT 2400` once it has sent scrambled ones at 2400 bit/s for
200 ms and heard 32 of them, or `CONNECT 1200` when the far end is V.22.

With automode, the answering modem sends ANSam in place of the answer tone
and the calling modem listens for either tone, as described under Automode
below. The line then leaves the answer tone to the pump.

#### DCD

`&C0`, the Hayes default, keeps DCD on. `&C1` makes it follow carrier: on at
`CONNECT`, off when the call ends, 200 ms after the result code so that the
computer can read it first.

A pseudoterminal has no DCD line, so dropping carrier hangs up the port
instead. The modem opens a fresh pseudoterminal, moves the link to it, and
closes the old one. Every program that had the old one open sees a hang-up,
which `pppd` reports as `Modem hangup`, and one that reopens the link gets the
new port. `pppd` notices at its next write, so within its LCP echo interval.
Rising DCD cannot be shown this way, and a terminal program such as `minicom`
has to reopen the port after each call, so `&C1` is for `pppd`.

What emulators see, as of 86Box's `char` layer and QEMU's 16550:

- 86Box's pipe backend, pointed at the pseudoterminal, reports DCD as "the
  descriptor is open". The hang-up drops it until the backend reconnects,
  which it does only with reconnect enabled. The guest sees DCD fall at the
  end of a call and nothing else.
- 86Box's host serial backend and QEMU's `-chardev serial` read DCD and RI
  with `TIOCMGET`, which a pseudoterminal does not answer.

`--cuse NAME` serves the port as `/dev/NAME` through CUSE instead, a
character device answered from user space, on Linux only. It answers the
termios ioctls, including `TCGETS2`, `TIOCMGET` with DCD, RI, DSR and CTS,
and `TIOCMSET`, `TIOCMBIS` and `TIOCMBIC`, which store DTR and RTS. RI is on
for 2 s of each 6 s ring. `checks.<linux>.cuse` reads the lines directly,
then boots a QEMU guest with `-chardev serial,path=/dev/ttySM0 -device
pci-serial`, which dials, and whose own driver reports `CD` only while the
call is up.

It needs the `cuse` module and access to `/dev/cuse`, and the node it
creates is root's, mode 0600, unless a udev rule says otherwise. `pppd`
cannot use it, since it is not a kernel tty and so takes no line
discipline.

Which port for what:

| Computer | Port |
|---|---|
| QEMU, on Linux | `--cuse`, with `-chardev serial` |
| 86Box, on Linux | `--cuse`, with the host serial backend |
| 86Box, elsewhere | `--pty`, with the pipe backend, reconnect on |
| `pppd` on the host | `--pty` |

#### Not modelled

A pseudoterminal has no DTR, so the computer cannot hang up by dropping it.
`pppd` hangs up with `+++` and `ATH` in its disconnect script, as on a line
without modem control. RI is not signalled either.

The pty is the DTE side and is much faster than the line. `softmodem` keeps a
small transmit buffer and stops reading from the pty when it is full. The
kernel pty buffer then fills and the writer blocks, which is the only
backpressure a pty offers. An unbounded buffer here turns into minutes of
queued latency at 300 bit/s.

## V.21 first, and why the standard rather than something ad hoc

Both ends are ours, so any FSK scheme would work between them. Implementing
V.21 to spec costs nothing extra and buys a path to interoperability with real
modems, because 300 bit/s FSK is what they all fall back to.

V.21 uses two channels so that both ends can transmit at once:

| Channel | Mark (1) | Space (0) |
|---|---|---|
| 1, originating | 980 Hz | 1180 Hz |
| 2, answering | 1650 Hz | 1850 Hz |

Checked against V.21 (11/88), which also sets the other figures the
implementation follows. The ITU download for it often fails; the same text is
in CCITT Blue Book Fascicle VIII.1, pages 65 to 69, which also holds V.22 and
V.25. Local copies go in `docs/specs/`, which is not committed.

- <https://www.itu.int/rec/T-REC-V.21/en>
- <https://search.itu.int/history/HistoryDigitalCollectionDocLibrary/4.260.43.en.1049.pdf>

Later editions come one at a time from
`https://www.itu.int/rec/dologin_pub.asp?lang=e&id=<id>%21%21PDF-E&type=items`,
where `<id>` is the edition, such as `T-REC-V.42-200203-I`. The page
`https://www.itu.int/rec/T-REC-<rec>/en` lists them. Take the main edition,
not an `!Cor1` or `!Amd1` beside it. The server often answers with an HTML
error page instead of the PDF, so check the file and try again, up to a dozen
times. The local copies:

| File | Edition |
|---|---|
| `CCITT-Blue-Book-Fascicle-VIII.1-1988.pdf` | V.1 to V.230 as of 1988, with V.21, V.22, V.22bis and V.42 |
| `V.8-200011.pdf` | V.8 (11/2000), CM and JM, ANSam |
| `V.14-199303-I.pdf` | V.14 (03/1993) |
| `V.25-199610-I.pdf` | V.25 (10/1996) |
| `V.32bis-199102-I.pdf` | V.32bis (02/1991), with the automode of its Annex A |
| `V.42-200203-I.pdf` | V.42 (03/2002) |
| `V.42bis-199001-I.pdf` | V.42bis (01/1990) |
| `V.250-200307-I.pdf` | V.250 (07/2003), including `+MS` |

The figures:

- Transmit level at most -13 dBm0 (§ 6).
- The demodulator tolerates ±12 Hz between received and nominal frequencies
  (§ 3).
- Carrier detect on above -43 dBm, off below -48 dBm, with at least 2 dB of
  hysteresis (§ 8.3). On the digital path these are taken as dBm0.
- Carrier detect on after 300 to 700 ms, off after 20 to 80 ms, the switched
  network figures of table 2.

V.21 allows any rate up to 300 bit/s and does not require a bit clock (§ 2,
§ 4): a hardware V.21 modem passes the tone decision straight to the UART.
This demodulator recovers bit timing at exactly 300 bit/s instead, so it only
carries 300 bit/s async.

Answer sequence, from V.25, to be checked against the ITU text before
implementation: after the call is answered, silence for 1.8 to 2.5 s, then
the 2100 Hz answer tone for 2.6 to 4.0 s, then 75 ms of silence, then
channel 2 mark. Phase reversals in the answer tone exist to disable network
echo cancellers and are not needed by V.21, so they are left out at first.
The line idles on mark whenever there is no data.

Bell 103 is the American equivalent and is worth adding once V.21 works. It
is not only a change of constants: mark is the *higher* tone, the opposite of
V.21, and the answer tone is 2225 Hz instead of 2100 Hz.

| Channel | Mark (1) | Space (0) |
|---|---|---|
| originating | 1270 Hz | 1070 Hz |
| answering | 2225 Hz | 2025 Hz |

### Real modems

The first real modem is a generic serial 56k modem, reported as a standard
AT modem by both Windows and Slackware. That says nothing about the chipset.
`ATI3`, `ATI4` and `ATI6` usually identify it.

A modern modem calls with V.8 and expects ANSam, a 2100 Hz tone with 15 Hz
amplitude modulation. In automode this modem sends ANSam and offers V.22bis
and V.21 in JM, so a V.8 caller should meet it at 2400 bit/s. A V.8 caller
that also offers V.32bis or V.34 will see only the modes both have. Should a
chipset still refuse, force it on the calling modem with a chipset-specific
command (for example `AT+MS=V22B` or `AT+MS=V21` on Rockwell parts), and
dial blind with `ATX3`.

#### Against spandsp

`softmodem-interop` checks the modem against spandsp, whose V.21 and answer
tones are in many real products. It links spandsp, so it only builds in the
dev shell, and the modem itself does not depend on it.

- spandsp demodulates our V.21, and we demodulate spandsp's, on both
  channels.
- spandsp's detector hears our answer tone as V.25 ANS.
- Our V.22 pump trains with spandsp's V.22 and carries data both ways,
  as caller and as answerer, with and without the guard tone. spandsp's
  V.22bis falls back to it at 1200 bit/s.
- Our V.22bis pump trains with spandsp's at 2400 bit/s as caller and as
  answerer, and falls back to 1200 bit/s when spandsp is held there.
- spandsp hears our ANSam with and without phase reversals, and we tell its
  four answer tones apart.
- Our automode negotiates V.8 with spandsp's as answerer and as caller,
  agrees on V.22bis, and then trains at 2400 bit/s with spandsp's V.22bis.
  With a spandsp that offers only V.21, V.8 agrees on V.21.
- Whole calls in automode: a V.25 caller that answers our USB1 as V.21, and
  an answerer that sends ANSam but speaks only V.21, both reach V.21. Each
  hears some of the other modulation first as noise, which is why those two
  tests allow junk before the data.
- Whole calls, with spandsp's parts as the far modem: we call one that
  answers with ANS, with ANS and phase reversals, and with V.8 ANSam and
  phase reversals, the tone a modern modem sends. A V.25 caller that waits for
  our answer tone and channel 2 calls us. Data crosses both ways each time.
  spandsp's side is plain start-stop there, so our V.42 falls back.
- Our V.42 against spandsp's, bit for bit with no modulation under them:
  with the detection phase and straight into LAPM, as caller and as
  answerer, 3000 octets each way. With one bit in 10007 flipped each way,
  REJ and timer recovery still deliver all of it. spandsp's V.42 sends only
  zeros until `v42_restart`, although its header declares a `v42_start`
  that the library does not export.

Those calls found a bug no test between two of our own modems could: the
answering modem reports `CONNECT` up to a second before the caller does, and
spandsp, like `pppd`, sends at once. The caller dropped what arrived before
its own `CONNECT` and misframed the first characters. It now keeps them.

What spandsp cannot stand in for is a modem's automode, the probing a real
modem does when it hears ANS instead of ANSam. That still needs the real
modem.

## V.22

Checked against V.22 (1988) in CCITT Blue Book Fascicle VIII.1, pages 69 to
81, with V.14 on pages 45 to 48 and V.2 on page 7. Of the three
alternatives, this is Alternative B mode ii): 1200 bit/s start-stop over the
constant carrier handshake. It does not do 600 bit/s, Alternative C mode v),
controlled carrier or the test loops.

| Channel | Carrier | Sent by |
|---|---|---|
| low | 1200 ± 0.5 Hz | the caller |
| high | 2400 ± 1 Hz | the answering modem, with a guard tone |

The figures:

- 600 baud ± 0.01%, two bits a symbol (§ 2.5.1). Each dibit is a phase
  change from the previous symbol, first bit on the left (table 1):

  | Dibit | Phase change |
  |---|---|
  | 00 | +90° |
  | 01 | 0° |
  | 11 | +270° |
  | 10 | +180° |

- The spectrum is a square root raised cosine with 75% roll-off, the same
  filter at both ends (§ 2.4).
- The 1800 ± 20 Hz guard tone goes with the high channel only, 6 ± 1 dB
  below its data power (§ 2.1, § 2.2). It is a national option. Europe uses
  it, so it is on here.
- Total transmitted power follows V.2: a mean of at most -13 dBm0, with the
  instantaneous power at most that of a 0 dBm0 sine (V.2 § 1.3). The
  high channel data is therefore about 1 dB below the low channel, since
  the guard tone takes its share.
- The receiver accepts ±7 Hz of carrier error (§ 2.6).
- Carrier detect on above -43 dBm, off below -48 dBm, with at least 2 dB of
  hysteresis (§ 3.3). On in 105 to 205 ms, off in 10 to 24 ms (table 3). It
  must not respond to the guard tones or to the answer tone during the
  handshake.
- Scrambler 1 + x⁻¹⁴ + x⁻¹⁷, self-synchronising (§ 5.1). After 64 ones in a
  row at its output, it inverts its next input, except during the handshake.
  The descrambler may do the same, and this one does not.

Handshake, after the V.25 answer sequence (§ 6.3.1, figure 4). Where the
spec gives a range, the modem uses the middle of it.

1. The answering modem sends unscrambled binary 1, with the guard tone.
2. The caller stays silent until it hears unscrambled binary 1 for
   155 ± 50 ms. Then it waits 456 ± 10 ms and sends scrambled binary 1.
3. When the answering modem hears scrambled binary 1 (or 0) for 270 ± 40 ms,
   it sends scrambled binary 1, waits 765 ± 10 ms, and turns carrier detect
   on.
4. When the caller hears scrambled binary 1 for 270 ± 40 ms, it turns
   carrier detect on and waits 765 ± 10 ms.
5. Both are in data. Until then, received data is held at binary 1. A loss
   and return of carrier after this does not start the handshake again
   (§ 6.3.1.2).

Start-stop characters cross the synchronous channel as V.14 describes. A
sender whose characters arrive up to 1% fast deletes a stop bit, at most one
in any eight characters. The receiver puts it back, and may shorten a stop
bit by up to 12.5% to keep up (V.14 § 7). A break of 2M + 3 or more start
bits, where M is the bits in a character, passes through unchanged (V.14
§ 7.3). The modem clocks out its own characters at exactly 1200 bit/s, so it
never deletes a stop bit, but its receiver must accept a character whose stop
bit is missing.

spandsp's V.22bis modem started at 1200 bit/s is the V.22 reference. Its
answer tone is a separate part, as with V.21.

## V.22bis

Checked against V.22 bis (1988) in the same fascicle, pages 82 to 97. This
is mode 2, 2400 bit/s start-stop, with its fallback to V.22 at 1200 bit/s.
It does the optional rate change of § 6.6, but not the test loops.

The line is V.22's: the same carriers, guard tone, levels, 600 baud, square
root raised cosine with 75% roll-off, scrambler, ±7 Hz, and carrier detect
thresholds (§§ 2, 3.3, 5).

- 2400 bit/s sends quadbits (§ 2.5.2.1). The first two bits change the
  quadrant as V.22 table 1 does. The last two pick one of four points in the
  new quadrant. In quadrant 1, on a grid of ±1 and ±3:

  | Bits 3 and 4 | Point |
  |---|---|
  | 00 | (1, 1) |
  | 01 | (3, 1) |
  | 10 | (1, 3) |
  | 11 | (3, 3) |

  The other quadrants are this one turned by 90°, 180° and 270°, so a
  receiver locked a quarter turn off still decodes the right bits.
- 1200 bit/s sends dibits as quadrant changes, always on the 01 point of the
  quadrant, which keeps it compatible with V.22 (§ 2.5.2.2).
- The scrambler's 64 ones guard runs at all times, handshake included, and
  resets its count when it fires (§ 5.1).
- Carrier detect goes off 40 to 65 ms after the signal falls below the
  threshold, or 10 to 24 ms in the V.22 fallback. After a dropout it comes
  back on in 40 to 205 ms (§ 3.2).

S1 is unscrambled double dibits 00 and 11 at 1200 bit/s for 100 ± 3 ms: the
phase turns by 90° and 270° in turn. The handshake at 2400 bit/s (§ 6.3.1.1,
figure 5), after the V.25 answer sequence:

1. The answering modem sends unscrambled binary 1 at 1200 bit/s, as in V.22.
2. The caller hears it for 155 ± 10 ms, stays silent 456 ± 10 ms, sends S1,
   then scrambled binary 1 at 1200 bit/s.
3. When the answering modem hears the end of S1, it sends S1 back, then
   scrambled binary 1 at 1200 bit/s.
4. Each end, counting from the end of the S1 it heard: at 450 ± 10 ms its
   receiver may make 16-way decisions, at 600 ± 10 ms it sends scrambled
   binary 1 at 2400 bit/s, and 200 ± 10 ms later it may send data.
5. Each end turns carrier detect on and takes data once it has heard 32 bits
   of scrambled binary 1 at 2400 bit/s in a row.

If an end hears scrambled binary 1 at 1200 bit/s for 270 ± 40 ms instead of
S1, the far end is a V.22 modem, and the V.22 handshake finishes at
1200 bit/s (§ 6.3.1.2, figures 6 and 7).

A retrain (§ 6.4, figure 8) starts when an end loses equalisation, or when
it hears S1 during data. It sends S1, then scrambled binary 1 at 1200 bit/s,
and goes on as from step 4. An end that sent S1 and hears none back within
1.2 s sends it again. After a loss of signal, received data stays held at
binary 1 for 100 ms after the signal returns, in case a retrain follows
(§ 6.5).

A rate change (§ 6.6, figure 9, table 4) is the same exchange with another
dibit after S1: 11 asks for 2400 bit/s, 01 or 10 for 1200 bit/s. Scrambled
binary 1 descrambles to 11, so a retrain is a rate change that asks for
2400 bit/s. The far end answers once it has heard 32 of the same dibit, with
S1 and the dibit it agrees to, and both go on at that rate, 450 ms after the
exchange for the receiver and 600 ms for the transmitter.

This modem, in these terms:

- It starts a retrain when its equaliser error stays above 0.045 for 300 ms.
  With random decisions that error can read at most (0.632)²/6 ≈ 0.067, so
  a higher threshold would never fire. A clean line reads about 0.0005.
- If it loses equalisation again within 10 s of a retrain, it asks for
  1200 bit/s instead. `ATO1` retrains by hand and asks for 2400 bit/s, so it
  also steps back up.
- An S1 that ends within 1 s of its own counts as the reply, as § 6.6.1 f)
  allows. Without that rule, two ends that start a retrain at once each take
  the other's reply for a new request and bounce S1 for ever.
- Carrier detect stays on through a retrain, as § 6.4 asks, so `S10` does
  not hang up.

spandsp's V.22bis modem at 2400 bit/s is the reference.

## Automode

Automode lets two modems meet at the best modulation both have. The
standard way is V.8 (11/2000); a modem that meets plain ANS instead falls
back to Annex A of V.32 bis (02/1991). V.250 § 6.4.1 turns it on with
`+MS=<carrier>,1`, `<carrier>` being the highest modulation to offer, and
recommends it on by default.

V.8, the figures:

- ANSam replaces ANS: 2100 ± 1 Hz, amplitude modulated by 15 ± 0.1 Hz
  between 0.8 and 1.2 of its mean, with phase reversals every 450 ± 25 ms
  when echo cancellers must be disabled (§ 7.2). The answering modem sends it
  for 5 ± 1 s unless CM arrives first (§ 8.2.2).
- A caller that hears ANSam stays silent for Te, at least 0.5 s and at least
  1 s to let echo cancellers disable, then sends CM until it has heard two
  identical JM (§ 8.1).
- CM goes in V.21 channel 1, JM in channel 2, both at 300 bit/s. Each
  sequence is ten ones, then the synchronisation bits 0000001111, then octets
  framed by a start and a stop bit, coded so that no HDLC flag appears
  (§ 5, table 1). CJ, three zero octets with their start and stop bits, ends
  CM (§ 3.5).
- The answering modem sends JM after two identical CM, listing the modes both
  have, and keeps sending it until CJ (§§ 7.4, 8.2.3).
- The octets this modem uses (§ 6, tables 2 to 4), bits b0 to b7 in the
  order sent, each framed by start and stop bits:
  - callf0, the call function: 1000 0 011, data.
  - modn0, the modulation modes category: 1010 0, then b5 PCM, b6 V.34
    duplex, b7 V.34 half-duplex, all 0 here.
  - modn1, an extension octet: b0 V.32bis/V.32, b1 V.22bis/V.22, b2 V.17,
    then 010, then b6 V.29, b7 V.27ter.
  - modn2, an extension octet: b0 V.26ter, b1 V.26bis, b2 V.23, then 010,
    then b6 V.23 half-duplex, b7 V.21.

- The mode in common with the lowest item number wins: V.22bis/V.22 is
  item 4, V.21 item 12 (§ 7.4). A caller that receives a JM with no mode in
  common may hang up after CJ.
- After CJ, each end is silent for 75 ± 5 ms, then starts the chosen
  modulation's own handshake (§§ 8.1.2, 8.2.3).

Annex A of V.32 bis, the fallback when either end does not speak V.8, covers
V.32 against V.22bis and V.22 only. Without V.32 it comes down to this: the
answering modem sends USB1 for Ta = 3000 ± 50 ms after the answer sequence
and goes on as V.22bis if it hears S1 or scrambled ones (§ A.2.2), and the
calling modem answers USB1 as V.22bis (§ A.2.1). No text covers V.21, so this
modem adds one step of its own: an answering modem that hears nothing during
Ta switches to V.21 channel 2 mark, and a calling modem that hears a pure
1650 Hz mark after the answer tone goes on as V.21.

The sequences, for this modem with automode on:

1. Answering: silence, ANSam for up to 5 s while listening for CM. On CM,
   V.8 as above, then the chosen modulation. On no CM, 75 ms of silence, then
   USB1 for 3 s as Annex A. On no answer to that, V.21 channel 2 mark.
2. Calling: on ANSam, V.8. On plain ANS, wait for USB1, which starts
   V.22bis with its own fallback to V.22, or for channel 2 mark, which starts
   V.21.

spandsp's V.8 module is the reference for CM, JM and CJ.

## V.42

Checked against V.42 (03/2002). That edition deletes Annex A, the MNP-like
alternative procedure, so LAPM is the only protocol. It keeps the detection
phase. V.42 runs where V.14 does, over V.22 and V.22bis. V.21 at 300 bit/s
stays plain.

What it gives: no start and stop bits, so up to about 20% more throughput at
the same rate, and a link that retransmits what line errors damage instead of
passing them to PPP. `CONNECT` waits until the link is up or has fallen back.

Detection phase (§ 7.2.1), once the pump is connected:

1. The originator sends the ODP: DC1 with even parity, ten ones, DC1 with odd
   parity, ten ones, for T400 = 750 ms or until it hears two adjacent ADPs.
2. The answerer sends mark and listens for four DC1s of alternating parity.
   It waits 1.5 s, not 750 ms, as it connects before the originator's carrier
   detect is on. § 9.1.1 lets an implementation change T400.
3. On the ODP, the answerer sends the ADP, `E`, ten ones, `C`, ten ones, at
   least ten times and until it hears flags (Appendix III.1).
4. On `EC`, the originator sends 16 flags, then XID, then SABME. On `E` and
   NUL, or nothing within T400, it falls back.
5. An answerer that hears three flags in a row, or a good frame, goes
   straight to LAPM. This is how it meets an originator with `+ES=2`.
6. An answerer that hears neither in time falls back. What it heard while it
   waited goes to the computer, as Appendix I.3 b) allows.

LAPM (§ 8), as this modem runs it:

- DLCI 0, modulo 128, the 16-bit FCS. XID offers N401 = 128 octets and
  k = 15 frames each way, and asks for no optional procedure. As responder,
  it takes the originator's values, up to 2048 octets and 127 frames, and
  agrees to no optional procedure. So SREJ, TEST and the 32-bit FCS are never
  in use. A SREJ or an undefined frame ends the call (§ 8.5.5).
- REJ on an N(S) sequence error, and polling with RR on T401 expiry. N400 is
  10. T401 is 1 s plus the time of two longest frames at the line rate, about
  2.1 s at 2400 bit/s, which covers the far end sending a whole frame first
  (Appendix IV).
- DISC, FRMR, an unsolicited DM, N400 failed polls, or an SABME after data
  end the call with `NO CARRIER`. An SABME before any data is a lost UA, and
  gets a UA again (§ 8.4.9.1).
- A BRK gets its BRKACK. The break does not reach the computer, as a
  pseudoterminal cannot show one.
- Stream mode (Appendix II d): an I frame takes what is queued when the last
  frame ends. In data mode the modem reads from the computer until two
  frames' worth wait, so frames are full during a transfer.

Not done: T403, own-receiver busy, suspending the timers during a retrain,
and DISC on hang-up. A pseudoterminal always takes what the modem writes, so
the receiver is never busy. The call's own end tells the far modem of a
hang-up. A retrain takes less than N400 times T401.

## Three things that decide whether it works

1. **Bit timing recovery.** Track the bit centre and correct on transitions.
   Between two instances of `softmodem` the sample rate is exact, since both
   ends count samples instead of clocking them. A real modem behind an ATA
   has its own crystal and the ATA's ADC has another, so their bit rates
   differ by some parts per million. Without tracking, the link works for a
   while and then dies, which is an unpleasant thing to debug after the fact.
   Build it from the start and test it with a resampled input.
2. **No playout clock.** See "Receive path".
3. **Test the DSP without SIP.** Modulate to demodulate in process, then
   through a file, then with injected loss, gain error, noise and a timing
   offset. Only then attach RTP. A modem that can only be tested by placing a
   phone call will not get finished.

### Receive path

The receiver does not play out on a local 8 kHz clock. It takes packets as
they arrive, puts them in order by RTP sequence number in a short fixed window
(a few packets), and fills any gap that the RTP timestamp shows with silence.
The demodulator then consumes samples as fast as they come.

This removes clock drift between the two hosts from the problem: a sender
that is slightly fast or slow only changes when packets arrive, not what
samples they contain. A playout clock would bring the drift back and need a
buffer that grows or shrinks, which is the adaptive jitter buffer excluded
above.

The transmitter still has to pace itself, 160 samples every 20 ms on the host
clock, because the PBX and any ATA downstream do play out in real time.

This is the proposed approach, not yet a decision. It needs checking that
nothing on the PBX side retimes the stream.

One lost 20 ms packet is 160 samples, which is six bit times at 300 bit/s.
That can corrupt two 8N1 characters, and the async framing can take a few
more characters to find the start bits again. PPP drops the frame and TCP
recovers.

## PPP

`softmodem` does not speak PPP. It carries bytes between the line and a pty,
and `pppd` runs on the pty. There is at most one call and one `pppd`, so the
answering side runs one long-lived `pppd` on a pty that `softmodem` keeps open
for its whole life.

The ISP's modem runs with `--init ATE0Q1S0=1`: no echo, no result codes, and
auto-answer on the first ring. Without `Q1`, `RING` and `CONNECT` reach the
waiting `pppd` as line noise. Its `pppd` needs no chat script.

The caller dials and hangs up the way it would with a real modem:

```
connect    "chat -v -t 60 '' ATZ OK ATDT0300 CONNECT '\c'"
disconnect "chat -v '' '\d\d+++\d\d\c' OK ATH0 OK"
```

A pty has no DCD, DTR or RTS/CTS, so `pppd` cannot see carrier and cannot hang
up by dropping DTR. The options that follow from that:

- `local`, since there are no modem control lines.
- `persist`, so `pppd` goes back to waiting after each call.
- `silent`, so it waits for the caller's LCP instead of sending its own into
  a line with nobody on it.
- `lcp-echo-interval` and `lcp-echo-failure`, since LCP echo is the only way
  it learns that a call has ended.
- `mru 296`, so a corrupted frame is cheap.
- `asyncmap 0`, since the path is 8-bit clean.
- `lcp-restart 15` and `ipcp-restart 15`. One LCP frame takes about a second
  each way, so a round trip is close to the 3 s default. With the default,
  each end retransmits before the answer arrives, the stale requests queue up,
  and one that arrives after LCP opens restarts the negotiation. This was seen
  in the VM test, not predicted.
- `noipv6` and `noccp`, since each extra control protocol adds a second or
  more of negotiation.

VJ header compression is a trade-off on this link, not a free gain. It saves
most of the 40 byte TCP/IP header, but after a lost frame the receiver
discards compressed packets until an uncompressed one arrives, which in
practice means one TCP retransmit timeout per lost frame. Measure before
enabling it.

At 300 bit/s, 30 bytes per second, LCP and IPCP exchange a few hundred bytes
in total. On a clean wire the VM test (`checks.<linux>.ppp`) measures:

- 6.6 s from `ATDT` to `CONNECT`, which is mostly the V.25 answer sequence.
- 10 s from `CONNECT` to an address on the first call.
- The same on every later call, now that `&C1` hangs up the port. Before
  that, the ISP's `pppd` never saw a call end, stayed in the old session, and
  the next call took about 25 s to renegotiate from there.
- About 6 s round trip for a 64 byte ping.

## Transport

The modem and terminal layers do not know about SIP. They talk to a transport
interface with four operations: dial a number, report an incoming call, send
and receive 20 ms frames of samples, and hang up. There are two
implementations.

**Wire.** RTP packets sent with `ezk-rtp` over UDP to a fixed peer, with no
signalling. Dial and answer are a minimal exchange on the same socket. This is
what two instances use during development. It is UDP and not TCP on purpose:
TCP hides loss and reordering, and then the receive path goes untested until
SIP arrives. The wire can inject loss, reordering and delay.

**SIP.** `ezk-sip-ua` for signalling, the same RTP code for media. Since the
media path is shared, this step adds only signalling.

- It registers one user with digest auth, over TCP to port 5060, and keeps
  the registration alive. The Contact carries `;transport=tcp` and the
  registration's own address, so the PBX sends incoming INVITEs back over
  the same connection and nothing has to listen.
- `ezk-sip-ua` runs without its `rtc` feature. Its `MediaBackend` trait is
  implemented here: the offer and answer hold PCMA only, and the audio runs
  on the shared RTP session. That session takes packets from any port on the
  far end's address, since the PBX need not send from the port it announced.
- A call that is refused with 486, 600 or 603 is `BUSY`. A dial given up
  before an answer sends CANCEL. An incoming call gets 180 Ringing, and a
  second one while it rings gets 486.
- Either end's hang-up ends the other: a BYE stops the RTP session, and the
  modem hanging up sends a BYE.

Against the Intraweb PBX, one modem dialling another's extension connects in
7.4 s from `ATDT`, most of which is the answer sequence.
`softmodem/tests/pbx.rs` holds the live tests, ignored unless asked for.

### WAV dumps

A wrapper around any transport writes each call to WAV files: one for the
samples sent and one for the samples received, after loss and gap fill.
Format is 8 kHz mono 16-bit PCM, which every player opens. The
received file shows exactly what the demodulator saw, so a failed call can be
replayed into the demodulator offline and turned into a test case. And you
can listen to the handshake.

Because it wraps the transport interface, it works the same on the wire and
on SIP.

### Speaker

`--speaker` plays each call on the host's default sound output through
`cpal`, which is CoreAudio on macOS and ALSA on Linux. Like the WAV dumps it
wraps the call, and it mixes both directions, as a real modem's speaker
hears the line. The samples are resampled from 8 kHz to the device rate by
linear interpolation. Each direction is held back 60 ms after it runs dry,
to ride out late frames, and is cut to 200 ms if it falls behind.

The modem sets the gain from `L` and `M` each time they or the call change.
Under the default `M1` the handshake is heard and the data is not. Without
`--speaker`, `L` and `M` are stored only.

## Milestones

The last milestone is far away and needs hardware that is not available yet.
Everything before it can be built and tested with two instances on one host.

1. V.21 DSP in isolation, unit tested with loss, noise, gain error and clock
   offset.
2. Transport interface, the wire and WAV dumps. Two instances, raw bytes
   across.
3. `pppd` on both ptys over the wire, an address, a ping across.
4. AT command interpreter on the serial port. The computer controls the
   modem with the basic Hayes command set, as with a real modem: `ATD` makes
   the modem place a call through its transport, `RING` and `ATA` answer
   one, `+++` and `ATO` leave and return to data mode. The answering side
   sends the V.25 answer tone. Until SIP exists, the transport is the wire.
5. SIP transport, two instances through the PBX. Done: data both ways,
   hang-up and busy, with PPP over it still to try.
6. Bell 103. Put off, as it is of little use in Europe.
7. A speaker: call audio on the host sound output, under `L` and `M`. Done.
8. V.22, selected with `AT+MS`. Done: between two instances, against
   spandsp, and with `pppd` in `checks.aarch64-linux.ppp-v22`.
9. V.22bis, selected with `AT+MS=V22B`, with its fallback to V.22 and its
   answer to a retrain. Done: between two instances, against spandsp in
   both roles and in fallback, and with `pppd` in
   `checks.aarch64-linux.ppp-v22bis`. It starts retrains, steps down to
   1200 bit/s when a retrain does not hold, and retrains on `ATO1`.
10. Automode: V.8, and V.32bis Annex A with a V.21 step of our own, on by
    default. Done: two instances meet at the best rate both have in every
    pairing with a fixed modulation, V.8 works against spandsp in both
    roles, and `pppd` runs over it in `checks.aarch64-linux.ppp-automode`.
11. V.42, on by default with `+ES=3,0,2`. Done between two instances, with
    fallback to plain data in each role and `\N2` hanging up without it, and
    with `pppd` over LAPM in `checks.aarch64-linux.ppp-v22bis`, and against
    spandsp's V.42 in both roles.
12. V.42bis.
13. A real modem behind the SPA2102 calling the answering side.

## Rejected, and why

Recorded so these are not re-investigated.

- **AudioSocket instead of a SIP stack.** Asterisk 22 has it in tree and the
  wire protocol is trivial, which would have removed the whole SIP layer. It
  was dropped because the calling side must be an independent endpoint, not
  something attached to the PBX process, and because the caller has to be
  replaceable by an ATA later.
- **spandsp for the DSP.** It carries V.21, V.23, Bell 103 and 202, V.22bis,
  the fax datapumps, and V.42 with V.42bis. It stops below V.32, so it does
  not reach the interesting speeds, and taking it means C behind FFI. It stays
  a black box that the interop tests call through its API. Its source is not
  read, because it is LGPL-2.1 and our code must not become a derived work.
  The ITU text is the only reference for how things work.
- **D-Modem as the calling side.** GPL-2.0, last pushed July 2023, genuinely
  used. Rejected for the `dsplibs.o` blob: non-free, 32-bit x86 only,
  unfixable, and it would have pinned the caller to an x86_64 host with
  multilib.
- **v90modem as the answering side.** See "Why not 56k".
- **`app_softmodem`.** The Asterisk module written for BTX and Minitel. Low
  speed only, out of tree, and an ABI rebuild against every Asterisk bump.

## Known cost

Without V.42bis the link gives up the factor of two or three on compressible
text that compression would have provided. Against a far end without V.42,
the link is raw async: PPP drops frames that fail FCS and TCP retransmits,
which is correct but wasteful.

## Open questions

- Receive path: is "no playout clock" safe, or does something between the
  two ends retime the stream?
- Which chipset is in the generic 56k modem, and does it reach V.21 from
  automode?
- Should the answering side authenticate SIP at all, beyond PPP's own PAP or
  CHAP?
