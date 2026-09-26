# softmodem

These documents record the design and the reasoning behind it, including the
options that were rejected, so that the rejected ones do not have to be
investigated twice. They say how the modem works, not how far the work has
got: the code, the tests and the git history hold that.

They cover the `softmodem` program only. The network side (Asterisk
endpoint, dial plan, PPP address pool, the ATA and the retro client) lives in
`vic-nix-config` and is out of scope here, apart from the interfaces this
program has to meet.

## Goal

A modem places an ordinary SIP call, a PPP session comes up over the
modulated audio, and the caller gets an address from the ISP.

The point is not bandwidth. The point is that the call is a real call, each
signal is the one its ITU text defines, so that a real modem could be at the
other end, and a machine from 1998 can dial in the way it would have dialled
the internet.

Non-goals:

- Speed. See the ladder in [scope](scope.md).

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
   Test it with a resampled input.
2. **No playout clock.** See "Receive path" in [the channel](channel.md).
3. **Test the DSP without SIP.** Modulate to demodulate in process, then
   through a file, then with injected loss, gain error, noise and a timing
   offset. A modem that can only be tested by placing a phone call will not
   get finished.

## Documents

- [Scope](scope.md): the speed ladder, V.34 and 56k, and what was rejected.
- [The channel](channel.md): what the audio path must give, and the receive
  path.
- [Terminal](terminal.md): the serial port, AT commands, result codes,
  S-registers and DCD.
- [Transport](transport.md): the wire, SIP, WAV dumps and the speaker.
- [Replay](replay.md): a recorded call played again, and what it did.
- [PPP](ppp.md): `pppd` on both ends.
- [Interop](interop.md): real modems and spandsp.
- [The ITU texts](specs.md): where to get them.
- The recommendations: [V.14](v14.md), [V.21](v21.md), [V.22](v22.md),
  [V.22bis](v22bis.md), [V.25](v25.md), [V.34](v34.md), [V.90](v90.md), [automode](automode.md),
  [V.8](v8.md), [V.8 bis](v8bis.md), [V.42](v42.md), [V.42bis](v42bis.md).
