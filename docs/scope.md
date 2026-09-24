# Scope

## Speed, and the ladder

Modulations, in order of how hard they are to implement:

| Standard | Rate | Nature | Verdict |
|---|---|---|---|
| V.21 | 300 bit/s | FSK, full duplex | yes |
| Bell 103 | 300 bit/s | FSK, full duplex | no, see [V.21](v21.md) |
| V.23 | 1200/75 | FSK, asymmetric | no, see below |
| V.22 | 1200 bit/s | DPSK, full duplex | yes |
| V.22bis | 2400 bit/s | QAM, adaptive equaliser | yes |
| V.32bis | 14.4k | QAM, echo cancellation | no |
| V.34 | 33.6k | QAM, line probing, echo cancellation | yes, see below |
| V.90 | 56k down | PCM codepoints | yes, both sides, see below |
| V.92 | 48k up | PCM codepoints both ways | later, see below |

The cliff is between FSK and everything after it, and it is not about bit
rate. FSK is two tones and a slicer. PSK and QAM need carrier recovery and
timing recovery, and QAM adds an adaptive equaliser. That is where a homegrown
modem stops being a weekend and starts being a season.

V.23 is FSK and so is cheap, but the 75 bit/s back channel makes PPP painful,
and many modems outside Europe do not implement it. V.22 is harder but is
supported by nearly every modem ever built, so it is the more useful step
above V.21.

## V.34

V.34 is the step after V.22bis: up to 33.6 kbit/s, and the upstream half of
V.90. It needs trellis coding, shell mapping, precoding, line probing, and
an echo canceller for the echo of its own signal from the far hybrid.

spandsp 3.1.1 has a V.34 modem, but only part of one. Two instances of it
exchange INFO0, send the line probing tones and start INFO1, then stop
before data, and its own test does not get past INFO0. So it is not used as
a reference: the ITU text is the only one until a real modem is at hand.
See [V.34](v34.md).

## 56k

No path here has an analogue segment. V.21 to V.34 cross A-law RTP, and
the A-law quantisation is noise on the line. What makes them real is that
each signal is the one its ITU text defines, so that a real modem behind an
ATA could be at the other end. V.90 meets the same goal: its codewords are
the ones V.90 defines.

V.90 has a digital modem, which sits on the digital network and sends PCM
codewords down, and an analogue modem, which receives them through at most
one D/A conversion and sends V.34 up. This modem is either one. The digital
modem needs V.34 to receive, and the analogue modem needs it to send.

PCM needs the octets to arrive unchanged. The digital modem puts codewords on
the wire as the linear values they decode to, which `alaw::encode` turns back
into the same octets. So nothing may scale, mix or filter the samples between
the pump and the encoder, and the PBX and the ATA must pass the octets
unchanged: no transcoding, no gain, no echo cancellation, no loss
concealment. An ATA's D/A runs on its own clock, so a slip in its jitter
buffer costs a retrain.

| Path | D/A conversions | Result |
|---|---|---|
| This modem to this modem, on the wire or over SIP | 0 | V.90, either end digital |
| This modem, digital, to a real modem behind an ATA | 1 | V.90 |
| This modem, analogue, to a real modem behind an ATA | 1, and the real modem is analogue | V.34 at most |

The last row is a limit of the far end: two analogue modems meet at V.34.

V.92 also sends codewords up. The analogue modem precodes its signal so
that the A/D in front of the digital modem lands on codewords. Between two
instances of this modem there is no A/D, and both ends are digital. V.92
comes after V.90.

Between two instances, V.90 is checked blind, as V.34 was. The test channel
must add what a real path adds, a D/A, a band limit, noise and a clock
offset, or the analogue modem's receiver has nothing to do. slmodemd is a
V.90 analogue modem, and checks the digital side. See
[interop](interop.md).

- The analogue side of V.90 is also in `dsplibs.o`, the 1.2 MB proprietary
  Smart Link binary in slmodemd. It is 32-bit x86 only, and it is used only
  in checks.
- The digital side exists only in `cryan209/v90modem`, a young
  single-author research tree with no licence file, a patched Conexant modem
  ROM, and 374 MB of vendored spandsp and PJSIP. Real work, genuinely
  interesting, not a dependency and not a reference.

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
- **v90modem as the answering side.** See "56k".
- **`app_softmodem`.** The Asterisk module written for BTX and Minitel. Low
  speed only, out of tree, and an ABI rebuild against every Asterisk bump.
