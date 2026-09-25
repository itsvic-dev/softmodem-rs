# The channel

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

## Receive path

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
When the queue to the transport is full, as after a stall of the host, it
makes no frame on that tick and sends later. A frame made and then dropped
would take samples out of the signal with no gap in the RTP timestamps, and
V.90 loses its data frame alignment on that.

This holds only if nothing between the two ends retimes the stream. That is
an open question.

One lost 20 ms packet is 160 samples, which is six bit times at 300 bit/s.
That can corrupt two 8N1 characters, and the async framing can take a few
more characters to find the start bits again. PPP drops the frame and TCP
recovers.
