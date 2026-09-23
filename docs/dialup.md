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
- V.42 error correction and V.42bis compression.

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
| `L0` to `L3` | Speaker volume. Stored only, as there is no speaker yet. |
| `M0` to `M2` | Speaker mode. Stored only. |
| `O` | Return to data mode from online command mode. |
| `Q0`, `Q1` | Result codes shown, suppressed. |
| `V0`, `V1` | Result codes as digits, as words. |
| `X0` to `X4` | Which result codes are used, see below. |
| `Z` | Hang up and reset to the stored profile. |
| `Sn=v`, `Sn?` | Write, read an S-register. |

Extended commands (`&`, `\`, `%` and `+` prefixes) are accepted and return
`OK` without effect, because chat scripts send chipset-specific strings such
as `AT&C1&D2` and fail on `ERROR`.

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
| 6 | `NO DIALTONE` | `X2`, `X4` |
| 7 | `BUSY` | `X3`, `X4` |

At 300 bit/s Hayes reports plain `CONNECT`, so `X1` adds nothing here.
Below the level that has them, `BUSY` and `NO DIALTONE` become `NO CARRIER`.
The default is `X4`. `NO DIALTONE` never happens. With `V1` each code is
framed by CR LF, with `V0` it is the digit and CR, both using `S3` and `S4`.

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
700 ms, and the demodulator keeps 400 ms.

#### Answer sequence

The answering modem sends silence for 2 s, the 2100 Hz answer tone for
3.3 s, 75 ms of silence, then channel 2 mark, and reports `CONNECT` when it
detects channel 1. The originating modem is silent until the answer tone has
ended and it detects channel 2, then sends channel 1 mark and reports
`CONNECT`. It ignores carrier while the answer tone lasts, because the tone
is close enough to channel 2 to trip carrier detect.

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
  with `TIOCMGET`, which a pseudoterminal does not answer. Both would show
  the real lines only if the port were a character device that does.

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
amplitude modulation. When it hears a plain V.25 answer tone instead, it falls
back to the older automode sequence, which probes several modulations before
it settles on V.21. Whether a given chipset reaches V.21 without help is not
known. Expect to force it on the calling modem with a chipset-specific command
(for example `AT+MS=V21` on Rockwell parts), and dial blind with `ATX3`.

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

### WAV dumps

A wrapper around any transport writes each call to WAV files: one for the
samples sent and one for the samples received, after loss and gap fill.
Format is 8 kHz mono 16-bit PCM, which every player opens. The
received file shows exactly what the demodulator saw, so a failed call can be
replayed into the demodulator offline and turned into a test case. And you
can listen to the handshake.

Because it wraps the transport interface, it works the same on the wire and
on SIP.

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
5. SIP transport, two instances through the PBX.
6. Bell 103.
7. A speaker: call audio on the host sound output, under `L` and `M`.
8. A real modem behind the SPA2102 calling the answering side.

## Rejected, and why

Recorded so these are not re-investigated.

- **AudioSocket instead of a SIP stack.** Asterisk 22 has it in tree and the
  wire protocol is trivial, which would have removed the whole SIP layer. It
  was dropped because the calling side must be an independent endpoint, not
  something attached to the PBX process, and because the caller has to be
  replaceable by an ATA later.
- **spandsp for the DSP.** It carries V.21, V.23, Bell 103 and 202, V.22bis,
  the fax datapumps, and V.42 with V.42bis. It stops below V.32, so it does
  not reach the interesting speeds, and taking it means C behind FFI. Its V.21
  remains the obvious reference implementation to read.
- **D-Modem as the calling side.** GPL-2.0, last pushed July 2023, genuinely
  used. Rejected for the `dsplibs.o` blob: non-free, 32-bit x86 only,
  unfixable, and it would have pinned the caller to an x86_64 host with
  multilib.
- **v90modem as the answering side.** See "Why not 56k".
- **`app_softmodem`.** The Asterisk module written for BTX and Minitel. Low
  speed only, out of tree, and an ABI rebuild against every Asterisk bump.

## Known cost

Without V.42 and V.42bis the link is raw async. PPP drops frames that fail
FCS and TCP retransmits, which is correct but wasteful, and on compressible
text it gives up the factor of two or three that V.42bis would have provided.

## Open questions

- Receive path: is "no playout clock" safe, or does something between the
  two ends retime the stream?
- Which chipset is in the generic 56k modem, and does it reach V.21 from
  automode?
- Should the answering side authenticate SIP at all, beyond PPP's own PAP or
  CHAP?
