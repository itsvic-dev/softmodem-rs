# V.22bis

Checked against V.22 bis (1988) in the same fascicle as [V.22](v22.md),
pages 82 to 97. This is mode 2, 2400 bit/s start-stop, with its fallback to
V.22 at 1200 bit/s. It does the optional rate change of § 6.6, but not the
test loops.

The line is V.22's: the same carriers, guard tone, levels, 600 baud, square
root raised cosine with 75% roll-off, scrambler, ±7 Hz, and carrier detect
thresholds (§§ 2, 3.3, 5).

- 2400 bit/s sends quadbits (§ 2.5.2.1). The first two bits change the
  quadrant as V.22 table 1 does. The last two pick one of four points in the
  new quadrant. In quadrant 1, on a grid of ±1 and ±3:

  | Bits 3 and 4 | Point |
  |---|---|
  | 00 | (1, 1) |
  | 01 | (3, 1) |
  | 10 | (1, 3) |
  | 11 | (3, 3) |

  The other quadrants are this one turned by 90°, 180° and 270°, so a
  receiver locked a quarter turn off still decodes the right bits.
- 1200 bit/s sends dibits as quadrant changes, always on the 01 point of the
  quadrant, which keeps it compatible with V.22 (§ 2.5.2.2).
- The scrambler's 64 ones guard runs at all times, handshake included, and
  resets its count when it fires (§ 5.1).
- Carrier detect goes off 40 to 65 ms after the signal falls below the
  threshold, or 10 to 24 ms in the V.22 fallback. After a dropout it comes
  back on in 40 to 205 ms (§ 3.2).

## Handshake

S1 is unscrambled double dibits 00 and 11 at 1200 bit/s for 100 ± 3 ms: the
phase turns by 90° and 270° in turn. The handshake at 2400 bit/s (§ 6.3.1.1,
figure 5), after the V.25 answer sequence:

1. The answering modem sends unscrambled binary 1 at 1200 bit/s, as in V.22.
2. The caller hears it for 155 ± 10 ms, stays silent 456 ± 10 ms, sends S1,
   then scrambled binary 1 at 1200 bit/s.
3. When the answering modem hears the end of S1, it sends S1 back, then
   scrambled binary 1 at 1200 bit/s.
4. Each end, counting from the end of the S1 it heard: at 450 ± 10 ms its
   receiver may make 16-way decisions, at 600 ± 10 ms it sends scrambled
   binary 1 at 2400 bit/s, and 200 ± 10 ms later it may send data.
5. Each end turns carrier detect on and takes data once it has heard 32 bits
   of scrambled binary 1 at 2400 bit/s in a row.

If an end hears scrambled binary 1 at 1200 bit/s for 270 ± 40 ms instead of
S1, the far end is a V.22 modem, and the V.22 handshake finishes at
1200 bit/s (§ 6.3.1.2, figures 6 and 7).

## Retrain and rate change

A retrain (§ 6.4, figure 8) starts when an end loses equalisation, or when
it hears S1 during data. It sends S1, then scrambled binary 1 at 1200 bit/s,
and goes on as from step 4. An end that sent S1 and hears none back within
1.2 s sends it again. After a loss of signal, received data stays held at
binary 1 for 100 ms after the signal returns, in case a retrain follows
(§ 6.5).

A rate change (§ 6.6, figure 9, table 4) is the same exchange with another
dibit after S1: 11 asks for 2400 bit/s, 01 or 10 for 1200 bit/s. Scrambled
binary 1 descrambles to 11, so a retrain is a rate change that asks for
2400 bit/s. The far end answers once it has heard 32 of the same dibit, with
S1 and the dibit it agrees to, and both go on at that rate, 450 ms after the
exchange for the receiver and 600 ms for the transmitter.

This modem, in these terms:

- It starts a retrain when its equaliser error stays above 0.045 for 300 ms.
  With random decisions that error can read at most (0.632)²/6 ≈ 0.067, so
  a higher threshold would never fire. A clean line reads about 0.0005.
- If it loses equalisation again within 10 s of a retrain, it asks for
  1200 bit/s instead. `ATO1` retrains by hand and asks for 2400 bit/s, so it
  also steps back up.
- An S1 that ends within 1 s of its own counts as the reply, as § 6.6.1 f)
  allows. Without that rule, two ends that start a retrain at once each take
  the other's reply for a new request and bounce S1 for ever.
- Carrier detect stays on through a retrain, as § 6.4 asks, so `S10` does
  not hang up.

## The pump

V.22bis shares the V.22 transmitter and front end. At 2400 bit/s the point
inside a quadrant is absolute, so its receiver adds an AGC, a 17-tap
equaliser at half-symbol spacing adapted by normalised LMS, and a
second-order phase locked loop, both driven by decisions. They train on the
scrambled ones at 1200 bit/s that follow S1. The handshake signals
themselves, S1 during data, and the V.22 fallback go through the
differential V.22 demodulator running beside it: a decision-directed
equaliser learns to flatten S1, which repeats every two symbols, and so
would erase it.

spandsp's V.22bis modem at 2400 bit/s is the reference.
