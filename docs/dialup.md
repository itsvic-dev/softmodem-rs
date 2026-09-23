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

Commands: `ATZ`, `AT&F`, `ATE0`/`ATE1`, `ATV0`/`ATV1`, `ATQ0`/`ATQ1`,
`ATS0=n`, `ATDT`, `ATDP`, `ATA`, `ATH`, `ATO`, and `+++` with guard times (one
second of silence before and after). Any other syntactically valid command
returns `OK`, because chat scripts send chipset-specific strings such as
`AT&C1&D2` and fail on `ERROR`. Result codes: `OK`, `ERROR`, `CONNECT`,
`RING`, `NO CARRIER`, `BUSY`, in both verbal and numeric form.

`ATDT<digits>` becomes a SIP URI. The dialling rules are configuration, not
code.

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

VJ header compression is a trade-off on this link, not a free gain. It saves
most of the 40 byte TCP/IP header, but after a lost frame the receiver
discards compressed packets until an uncompressed one arrives, which in
practice means one TCP retransmit timeout per lost frame. Measure before
enabling it.

At 300 bit/s, 30 bytes per second, LCP and IPCP exchange a few hundred bytes
in total, so expect about 10 to 20 s between `CONNECT` and an address on a
clean line, and more with retransmits.

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

## Milestones

The last milestone is far away and needs hardware that is not available yet.
Everything before it can be built and tested with two instances on one host.

1. V.21 DSP in isolation, unit tested with loss, noise, gain error and clock
   offset.
2. Transport interface and the wire. Two instances, raw bytes across.
3. `pppd` on both ptys over the wire, an address, a ping across.
4. AT layer, so `ATDT` and `ATA` work on the wire transport.
5. SIP transport, two instances through the PBX.
6. Bell 103.
7. A real modem behind the SPA2102 calling the answering side.

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
