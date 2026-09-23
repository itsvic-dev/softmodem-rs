# Dial-up over the Intraweb

**Status:** draft, nothing built yet. This records a design and the reasoning
behind it, including the options that were rejected, so that the rejected ones
do not have to be investigated twice.

## Goal

vic!ISP offers dial-up. A modem places an ordinary call on the Intraweb
telephone network described in `voip.md`, a PPP session comes up over the
modulated audio, and the caller gets an address from the ISP pool.

The point is not bandwidth. The point is that the call is a real call, the
carrier is a real carrier, and a machine from 1998 can reach the island the
way it would have reached the internet.

Non-goals:

- Speed. The first target is 300 bit/s.
- PSTN access. Same island as `voip.md`, same reasons.
- 56k. See "Why not 56k" below. The conclusion is not "impossible", it is
  "not reachable with anything that exists and is maintainable".
- Emergency calling, again.

## Why the Intraweb is a good fit, and where it is not

Everything `voip.md` says about NAT still applies, and the SIP profile there
already forces most of what a modem needs: PCMA is mandatory, media is PBX to
PBX, and `direct_media=no` keeps the path stable for the life of the call.

What a modem needs on top of that:

- **G.711 only, no transcoding.** An Opus leg anywhere destroys the waveform.
  The profile lists Opus first, so a modem endpoint must pin `allow=alaw` and
  nothing else.
- **No voice processing.** No VAD, no silence suppression, no comfort noise,
  no echo cancellation. All of these are designed to discard exactly the
  signal a modem is sending.
- **No adaptive jitter buffer.** A buffer that resizes mid-call reorders the
  sample stream, which no demodulator survives. Fixed depth, or none.
- **Loss matters, latency does not.** A modem is indifferent to half a second
  of delay and intolerant of a dropped packet. This is the opposite of the
  tuning a PBX ships with.

## Speed, and the ladder

Modulations, in order of how hard they are to implement:

| Standard | Rate | Nature | Verdict |
|---|---|---|---|
| V.21, Bell 103 | 300 bit/s | FSK, full duplex | first target |
| V.23 | 1200/75 | FSK, asymmetric | plausible follow-up |
| V.22bis | 2400 bit/s | QAM, adaptive equaliser | a different project |
| V.32bis | 14.4k | QAM, echo cancellation | no |
| V.34 | 33.6k | QAM, line probing | no |
| V.90 | 56k down | PCM codepoints | see below |

The cliff is between V.23 and V.22bis, and it is not about bit rate. FSK is
two tones and a slicer. QAM needs carrier recovery, timing recovery and an
adaptive equaliser, and that is where a homegrown modem stops being a
weekend and starts being a season.

### Why not 56k

V.90 works by having one end sit digitally on the network and inject PCM
codepoints directly, with exactly one D/A conversion on the path. An A-law
RTP stream over the overlay has *no* analogue segment at all, so the channel
is better than any real phone line: roughly 38 dB of SNR across the full
4 kHz, with no 3.1 kHz telco bandpass. Shannon puts that somewhere around
40 to 50 kbit/s. The physics is not the problem.

The problem is that no maintainable implementation exists at either end.

- The analogue client side of V.90 exists in exactly one place in the free
  world: `dsplibs.o`, a 1.2 MB proprietary Smart Link binary vendored into
  `slmodem` and from there into D-Modem. It is 32-bit x86 only, cannot be
  fixed, and cannot be improved.
- The digital server side exists only in `cryan209/v90modem`, a seven month
  old single-author research tree with `AGENTS.md` and `CLAUDE.md` at its
  root, no licence file, a patched Conexant modem ROM, and 374 MB of vendored
  spandsp and PJSIP. Real work, genuinely interesting, not a dependency.

So 56k reduces to "get two things we cannot maintain to talk to each other".
It stays on the shelf as a curiosity, not a plan.

## Architecture

```
retro client ── COM1 ── pty ── softmodem ── SIP/RTP alaw ──> pbx.vic.iw
                                                                  │
                                                                  v
                                                softmodem (answer) ── pty ── pppd
```

One program, `softmodem`, in both roles. It is a SIP user agent in its own
right, not an Asterisk module and not an AudioSocket client, so the calling
side is a normal endpoint that can live anywhere and can later be replaced by
real hardware without the answering side changing.

## The `softmodem` program

Its own repo, not part of this one. Rust, matching `intraweb-isp`. Three
layers, deliberately separable.

**SIP and RTP.** The `ezk` crate family covers this in pure Rust:
`ezk-sip-ua` (0.9.1, April 2026) with `ezk-rtp` (0.2.1, July 2026), plus the
SIP types, SDP and auth crates alongside them. `rvoip` is a newer alternative
with far less use, and `rsip` is parse-only and stale since 2022. A-law is a
lookup table, so there is no codec dependency.

This layer is the reason the design changed. Two years ago it would have
meant binding PJSIP from Rust, which is most of what D-Modem and v90modem
consist of.

**Modem.** A pure function over sample buffers, `&[i16] -> bits` and
`bits -> Vec<i16>`. No IO, no SIP, no async, no globals. The modulator is an
NCO switching between two frequencies. The demodulator is a bandpass, a
discriminator or a pair of correlators, a slicer, and bit timing recovery.

**Terminal.** `openpty`, 8N1 async framing, and an AT interpreter: `ATZ`,
`ATE`, `ATDT`, `ATA`, `ATS0`, `ATH`, `+++`, with the result codes `OK`,
`CONNECT`, `RING`, `NO CARRIER` and `BUSY`. `ATDT<digits>` becomes a SIP URI
under the dialling rules already written down in `voip.md`, which is the same
one regex.

### V.21 first, and why the standard rather than something ad hoc

Both ends are ours, so any FSK scheme would work between them. Implementing
V.21 to spec instead costs nothing extra and buys interoperability with every
modem ever built, because 300 bit/s FSK is what they all fall back to. The
ISA card, when it arrives, dials in with no further work.

V.21 uses two channels so that both ends can transmit at once. From memory,
and to be checked against the ITU text before implementation:

| Channel | Mark | Space |
|---|---|---|
| 1, originating | 980 Hz | 1180 Hz |
| 2, answering | 1650 Hz | 1850 Hz |

The answering side sends 2100 Hz answer tone first, then its channel 2 mark.

Bell 103 is the American equivalent at 1070/1270 and 2025/2225 and is worth
adding once V.21 works, since it is the same code with different constants.

### Three things that decide whether it works

1. **Bit timing recovery.** Each end derives 8 kHz from its own clock and
   they drift apart. Do not assume sample alignment. Track the bit centre and
   correct on transitions. Without this the link works for half a minute and
   then dies, which is an unpleasant thing to debug after the fact.
2. **Fixed jitter buffer.** Pick a depth, never adapt it, conceal loss with
   silence. One lost 20 ms packet costs six bit times at 300 bit/s, so it
   destroys a character. PPP drops the frame and TCP recovers.
3. **Test the DSP without SIP.** Modulate to demodulate in process, then
   through a file, then with injected loss, gain error and timing offset.
   Only then attach RTP. A modem that can only be tested by placing a phone
   call will not get finished.

### First integration test

Point the modem at extension `100`. The echo test reflects the audio, so one
instance proves SIP, RTP, A-law, the jitter buffer, and its own demodulator
against its own modulator, across the real network, with nothing else
running.

## Milestones

1. DSP in isolation, unit tested with impairments.
2. SIP user agent that calls `100` and decodes its own echo.
3. Two instances, one answering on `de-fra01`, raw bytes across.
4. `pppd` on both ptys, an address from the pool, a ping across.
5. AT layer, so `ATDT` works from a terminal.
6. 86Box with Slackware 8 attached to the calling pty.
7. V.23 at 1200/75, if the appetite survives.

## PPP

`ppp` is 2.5.2 in nixpkgs, so nothing needs packaging for this half.

At 300 bit/s, LCP and IPCP exchange a few hundred bytes each way, so expect
one to two minutes between `CONNECT` and an address. Set `mru 296` so a
corrupted frame is cheap, enable VJ header compression, and set
`asyncmap 0`, since the path is 8-bit clean and there is no PSTN in it to
eat control characters.

Addressing is unsettled. The pool is `10.32.0.0/24`, which the node agent on
`de-fra01` claims through `guardPrefixes`, and it is not yet known whether it
tolerates a `pppd` assigned `/32` inside that range. Either a sub-range is
carved out that the agent does not manage, or `intraweb-isp` grows a second
allocation path. With one subscriber this can also start as a static address
and be deferred.

## Asterisk side

The dial-in number is an extension on carrier `1`, on the existing PBX in
`de-fra01/pbx.nix`, rather than standing up carrier `3`. Extension `0300` is
proposed, for the line rate, but nothing depends on it.

The endpoint for the answering `softmodem` differs from the user endpoints
already in that file:

- `disallow=all` then `allow=alaw` only. No Opus on this endpoint.
- `direct_media=no`, as everywhere else.
- No jitter buffer on the channel.
- `context` of its own, not `from-local`, so the POP is not reachable as a
  side effect of the extension pattern matches.

Note that `restartIfChanged = false` upstream means the existing
`reloadTriggers` list has to grow whenever a config file is added, or changes
are silent no-ops on switch.

## Hardware, later

The eventual setup is an ISA modem in a retro machine, an SPA2102 providing
the analogue line, and the same answering `softmodem` unchanged. The SPA2102
is Sipura firmware and exposes everything needed:

- `Modem Line: yes`, which disables the echo canceller and silence
  suppression for that port.
- `Echo Canc Enable`, `Echo Canc Adapt Enable`, `Echo Supp Enable`,
  `Silence Supp Enable` all off explicitly, rather than trusting the above.
- `Preferred Codec: G711a` with `Use Pref Codec Only: yes`.
- `Network Jitter Level: very low`, `Jitter Buffer Adjustment: disable`.
- `FAX Enable T38: no`, `FAX CED Detect Enable: no`,
  `FAX CNG Detect Enable: no`. Left on, CED detection fires on the answering
  modem's tone and the ATA switches into fax handling mid-call.
- `Call Waiting` and `Three Way Calling` off, so nothing can inject a tone
  into an established data call.
- A dial plan replacing the stock NANP one, matching the Intraweb numbering.

On the modem itself, dial blind with `ATX3`. VoIP call progress tones rarely
match what a 1990s modem expects, and `ATX4` will report `NO DIALTONE` on a
call that was fine.

The SPA2102 firmware may be UDP only. That is acceptable: the TCP default in
`voip.md` exists for MTU reasons on carrier to carrier links, and this is a
local subscriber leg.

## The retro client

86Box hosts the retro machine. Its built-in modem is a virtual one that maps
`ATDT` to a TCP connection, with a phonebook and optional Telnet emulation.
There is no audio in it, so it cannot be used here.

What is usable is the serial port device list. A COM port can be a named
pipe, and on Linux and macOS that path may point at a character device such
as a pseudoterminal. So COM1 attaches directly to the `softmodem` pty and the
guest believes it has a modem. 86Box v6.0 reworked the serial device
selector, and there is a history of passthrough bugs including a Linux
one-way issue, so this wants testing early rather than assuming.

The guest is Slackware 8. `pppd` there is 2.4.x with PAP, CHAP-MD5 and VJ
compression, so the client side is a chat script and an options file. The
awkward part is not PPP but ISA Plug and Play: the kernel only autodetects
COM1 and COM2, so an ISA card needs `isapnptools` and `setserial`. Both ship
with Slackware 8.

Nothing on that machine can complete a modern TLS handshake, so a dial-up
user sees a smaller island than a WireGuard subscriber does: plain HTTP,
gopher, telnet, finger, IRC. That suits 300 bit/s.

## Rejected, and why

Recorded so these are not re-investigated.

- **AudioSocket instead of a SIP stack.** Asterisk 22 has it in tree and the
  wire protocol is trivial, which would have removed the whole SIP layer. It
  was dropped because the calling side must be an independent endpoint, not
  something attached to the PBX process, and because the caller has to be
  replaceable by an ATA later.
- **spandsp for the DSP.** It carries V.21, V.23, Bell 103 and 202, V.22bis,
  the fax datapumps, and V.42 with V.42bis. It stops below V.32, so it does
  not reach the interesting speeds, and taking it means writing C. Its V.21
  remains the obvious reference implementation to read.
- **D-Modem as the calling side.** 142 stars, GPL-2.0, last pushed July 2023,
  genuinely used. Rejected for the `dsplibs.o` blob: non-free, 32-bit x86
  only, unfixable, and it would have pinned the caller to an x86_64 host with
  multilib. It is also originate-only, which happens to suit the role.
- **v90modem as the answering side.** See "Why not 56k".
- **`app_softmodem`.** The Asterisk module written for BTX and Minitel. Low
  speed only, out of tree, and an ABI rebuild against every Asterisk bump.
- **Two real modems on the two FXS ports of the SPA2102.** Zero code, V.34
  with V.42bis, and historically the correct way to run a POP. It needs a
  second modem, a host with a serial port next to the ATA, and `tastypi` is
  the only physical machine available. Deferred on cost, not on merit. It
  remains the fastest path to a fast link if buying hardware becomes
  acceptable.
- **86Box's own virtual modem over TCP.** Works today, feels like dial-up,
  and has no VoIP in it at all. Useful only as a way to develop the Slackware
  side in parallel.

## Known cost

A homegrown V.21 will not interoperate with a real modem's error correction
or compression, because V.42 and V.42bis are not in scope. The link is raw
async. PPP drops frames that fail FCS and TCP retransmits, which is correct
but wasteful, and on compressible text it gives up the factor of two or three
that V.42bis would have provided.

## Open questions

- Does the node agent tolerate a `pppd` assigned `/32` inside
  `guardPrefixes`, or does dial-up need its own allocation path in
  `intraweb-isp`?
- Can a leaf client register to the PBX across the mesh? This is already open
  in `voip.md` and everything here depends on it.
- Where does the calling `softmodem` run for the first two-ended test?
  `pl-waw01` and `it-mil01` are the x86_64 nodes in the mesh, `tastypi` is
  aarch64, and none of that matters once the program is pure Rust.
- Should the answering side authenticate at all, beyond PPP's own PAP or
  CHAP? The SIP trust model in `voip.md` is address based, and one subscriber
  does not justify more.
- Is a second carrier code worth it later, so that dial-up sits under vic!ISP
  as carrier `3` rather than as an extension on carrier `1`?
