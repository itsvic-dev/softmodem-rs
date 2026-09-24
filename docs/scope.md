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
| V.90 | 56k down | PCM codepoints | the digital side only, see below |
| V.92 | 48k up | PCM codepoints both ways | no, see below |

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

V.90 works by having one end sit digitally on the network and inject PCM
codepoints directly, with exactly one D/A conversion on the path. An A-law
RTP stream has *no* analogue segment at all. Without loss, the channel is a
64 kbit/s digital pipe, and a program at each end could send raw bytes as
samples. That is excluded by the goal: it is not a carrier.

With a real modem behind the ATA, the ATA's D/A is the one conversion, as a
line card's would be. So this modem can be the digital side of V.90 against
a real analogue client. It cannot be the analogue client: two analogue
modems meet at V.34 at most, and no digital server is on the network.

The digital side of V.90 sends codewords down and receives V.34 up, so it
needs V.34 first. It puts codewords on the wire as the linear values they
decode to, which `alaw::encode` turns back into the same octets. That
holds only if nothing scales, mixes or filters the samples between the pump
and the encoder, and if the PBX and the ATA pass the octets unchanged: no
transcoding, no gain, no echo cancellation, no loss concealment. The ATA's
D/A runs on its own clock, so a slip in its jitter buffer costs a retrain.

V.92 also sends codewords up: the client precodes its signal so that the
ATA's A/D lands on codewords, where V.34 upstream takes the A/D's
quantisation noise. The digital side of that is out of scope here.

V.90 can only be checked against a real modem. The free implementations
are no reference either.

- The analogue client side of V.90 exists in exactly one place in the free
  world: `dsplibs.o`, a 1.2 MB proprietary Smart Link binary vendored into
  `slmodem` and from there into D-Modem. It is 32-bit x86 only, cannot be
  fixed, and cannot be improved.
- The digital server side exists only in `cryan209/v90modem`, a young
  single-author research tree with no licence file, a patched Conexant modem
  ROM, and 374 MB of vendored spandsp and PJSIP. Real work, genuinely
  interesting, not a dependency.

Neither is a dependency, and neither is a reference.

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
