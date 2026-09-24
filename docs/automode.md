# Automode

Automode lets two modems meet at the best modulation both have. The
standard way is [V.8](v8.md) (11/2000), with [V.8 bis](v8bis.md) before it;
a modem that meets plain ANS instead falls back to Annex A of V.32 bis
(02/1991). V.250 § 6.4.1 turns it on with `+MS=<carrier>,1`, `<carrier>`
being the highest modulation to offer, and recommends it on by default.

## V.32 bis Annex A

Annex A of V.32 bis, the fallback when either end does not speak V.8, covers
V.32 against V.22bis and V.22 only. Without V.32 it comes down to this: the
answering modem sends USB1 for Ta = 3000 ± 50 ms after the answer sequence
and goes on as V.22bis if it hears S1 or scrambled ones (§ A.2.2), and the
calling modem answers USB1 as V.22bis (§ A.2.1). No text covers V.21, so this
modem adds one step of its own: an answering modem that hears nothing during
Ta switches to V.21 channel 2 mark, and a calling modem that hears a pure
1650 Hz mark after the answer tone goes on as V.21.

## The sequences

The sequences, for this modem with automode on:

1. Answering: 400 ms of silence, then CRe, and silence to 2 s. If the caller
   answers CRe, V.8 bis. Then, or at 2 s if nothing answered, ANSam
   for up to 5 s while listening for CM. On CM, V.8, then the
   chosen modulation. On no CM, 75 ms of silence, then USB1 for 3 s as
   Annex A. On no answer to that, V.21 channel 2 mark.
2. Calling: on CRe, V.8 bis. On ANSam, V.8. On plain ANS, wait for USB1,
   which starts V.22bis with its own fallback to V.22, or for channel 2
   mark, which starts V.21.

With a fixed modulation there is nothing to agree on, so neither V.8 bis nor
V.8 runs. A caller without V.8 bis hears CRe as a short, quiet tone and
ignores it, so it costs it nothing. Between two instances, V.8 bis puts ANSam about 0.4 s
later.
