# Transport

The modem and terminal layers do not know about SIP. They talk to a transport
interface with four operations: dial a number, report an incoming call, send
and receive 20 ms frames of samples, and hang up. There are two
implementations.

**Wire.** RTP packets sent with `ezk-rtp` over UDP to a fixed peer, with no
signalling. Dial and answer are a minimal exchange on the same socket. This is
what two instances use during development. It is UDP and not TCP on purpose:
TCP hides loss and reordering, and then the receive path goes untested
without SIP. The wire can inject loss, reordering, slips and stalls.

It can also act as the path of a VoIP provider, as real calls through one
showed it: `--delay` holds each frame back, `--conceal` sends a frame as
silence, as a gateway conceals a lost packet, and `--noise` adds noise
before the samples are coded, as a path that decodes and codes again.
`--gateway-after` adds `--gateway-noise` to all that it sends once the far
end has been quiet that long, as a gateway that took the call for voice
again. On real calls that happened after about 6 s of quiet from the
analogue modem, and stayed.

**SIP.** `ezk-sip-ua` for signalling, the same RTP code for media. As the
media path is shared, SIP adds only signalling.

- It registers one user with digest auth, over TCP to port 5060, and keeps
  the registration alive. The Contact carries `;transport=tcp` and the
  registration's own address, so the PBX sends incoming INVITEs back over
  the same connection and nothing has to listen.
- `--protocol udp` registers over UDP instead, for providers that speak
  nothing else. The socket binds the local address that routes to the
  registrar, as the Via, the Contact and the SDP carry it. Every 25 s a
  CRLF keepalive goes to the registrar, to keep a NAT's binding open for
  incoming INVITEs. Our Via has no `rport`, so behind NAT the provider must
  send its responses back to the address they came from.
- `ezk-sip-ua` runs without its `rtc` feature. Its `MediaBackend` trait is
  implemented here: the offer and answer hold PCMA only, and the audio runs
  on the shared RTP session. That session takes packets from any port on the
  far end's address, since the PBX need not send from the port it announced.
- A call that is refused with 486, 600 or 603 is `BUSY`. A dial given up
  before an answer sends CANCEL. An incoming call gets 180 Ringing, and a
  second one while it rings gets 486.
- Either end's hang-up ends the other: a BYE stops the RTP session, and the
  modem hanging up sends a BYE.

Open question: should the answering side authenticate SIP at all, beyond
PPP's own PAP or CHAP?

## SIP and RTP

The `ezk` crate family covers this in pure Rust: `ezk-sip-ua` with `ezk-rtp`,
plus the SIP types, SDP and auth crates alongside them. `rvoip` is a newer
alternative with far less use, and `rsip` is parse-only and stale since
2022. A-law is a lookup table, so there is no codec dependency.

## WAV dumps

A wrapper around any transport writes each call to WAV files: one for the
samples sent and one for the samples received, after loss and gap fill.
Format is 8 kHz mono 16-bit PCM, which every player opens. The
received file shows exactly what the demodulator saw, so a failed call can be
replayed into the demodulator offline and turned into a test case. And you
can listen to the handshake.

Because it wraps the transport interface, it works the same on the wire and
on SIP.

## Speaker

`--speaker` plays each call on the host's default sound output through
`cpal`, which is CoreAudio on macOS and ALSA on Linux. Like the WAV dumps it
wraps the call, and it mixes both directions, as a real modem's speaker
hears the line. The samples are resampled from 8 kHz to the device rate by
linear interpolation. Each direction is held back 60 ms after it runs dry,
to ride out late frames, and is cut to 200 ms if it falls behind.

The modem sets the gain from `L` and `M` each time they or the call change.
Under the default `M1` the handshake is heard and the data is not. Without
`--speaker`, `L` and `M` are stored only.
