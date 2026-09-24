# Scope

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

## Why not 56k

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
