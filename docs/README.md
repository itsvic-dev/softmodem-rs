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
- 56k. See "Why not 56k" in [scope](scope.md).

## Architecture

```
caller ── pty ── softmodem ── SIP/RTP alaw ──> PBX ──> softmodem (answer) ── pty ── pppd
```

One program, `softmodem`, in both roles. It is a SIP user agent in its own
right, not an Asterisk module and not an AudioSocket client. Thus the calling
side is a normal endpoint that can live anywhere, and it can later be replaced
by a real modem behind an ATA without a change to the answering side.

Three layers, deliberately separable: [transport](transport.md), modem,
[terminal](terminal.md).

### Modem

A stateful streaming processor over linear samples: A-law is decoded to `i16`
at the RTP boundary and never seen by the DSP. The modulator and demodulator
each hold their state across buffers (filter history, oscillator phase, bit
timing) and expose one method, roughly `process(&mut self, input) -> output`.
No IO, no SIP, no async, no globals.

Each modulation is a data pump: its own handshake, modulator and demodulator
behind one trait, in its own file, chosen for each call by `+MS`. The line
around it holds only what all of them share: the V.25 answer sequence, the
link over the pump's bits, and the bytes that arrive before `CONNECT`. The
link, in `softmodem-link`, is V.42 or plain start-stop characters. See
[V.42](v42.md).

## Three things that decide whether it works

1. **Bit timing recovery.** Track the bit centre and correct on transitions.
   Between two instances of `softmodem` the sample rate is exact, since both
   ends count samples instead of clocking them. A real modem behind an ATA
   has its own crystal and the ATA's ADC has another, so their bit rates
   differ by some parts per million. Without tracking, the link works for a
   while and then dies, which is an unpleasant thing to debug after the fact.
   Build it from the start and test it with a resampled input.
2. **No playout clock.** See "Receive path" in [the channel](channel.md).
3. **Test the DSP without SIP.** Modulate to demodulate in process, then
   through a file, then with injected loss, gain error, noise and a timing
   offset. Only then attach RTP. A modem that can only be tested by placing a
   phone call will not get finished.

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
12. V.42bis, on by default with `+DS=3,0,2048,32`. Done between two
    instances in each direction and in none, against spandsp's codec,
    negotiated with spandsp's V.42, and with `pppd` over it in
    `checks.aarch64-linux.ppp-v22bis`.
13. V.8 bis in automode, before V.8. Done between two instances in
    transaction 12, with the other transactions checked message by
    message, and against spandsp, which does not answer CRe.
14. A real modem behind the SPA2102 calling the answering side.

## Open questions

- Receive path: is "no playout clock" safe, or does something between the
  two ends retime the stream?
- Which chipset is in the generic 56k modem, and does it reach V.21 from
  automode?
- Should the answering side authenticate SIP at all, beyond PPP's own PAP or
  CHAP?

## Documents

- [Scope](scope.md): the speed ladder, why not 56k, and what was rejected.
- [The channel](channel.md): what the audio path must give, and the receive
  path.
- [Terminal](terminal.md): the serial port, AT commands, result codes,
  S-registers and DCD.
- [Transport](transport.md): the wire, SIP, WAV dumps and the speaker.
- [PPP](ppp.md): `pppd` on both ends.
- [Interop](interop.md): real modems and spandsp.
- [The ITU texts](specs.md): where to get them.
- The recommendations: [V.14](v14.md), [V.21](v21.md), [V.22](v22.md),
  [V.22bis](v22bis.md), [V.25](v25.md), [automode](automode.md),
  [V.8](v8.md), [V.8 bis](v8bis.md), [V.42](v42.md), [V.42bis](v42bis.md).
