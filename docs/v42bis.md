# V.42bis

Checked against V.42 bis (01/1990), with its Annex A and V.42 Table 11b for
the XID encoding. It runs over LAPM only, never over plain data.

## Negotiation

Negotiation (§ 5.1, Annex A), in the XID that precedes SABME:

- The caller proposes P0, P1 and P2 from `+DS`: by default both directions,
  2048 codewords and strings of 32. Appendix II calls 2048 a good choice.
  V.250 recommends 6 for the string, but 32 does better on repetitive data.
- The answerer replies with the directions both want and the lower of each
  value. A P1 below 512 or a P2 outside 6 to 250 is a procedural error and
  ends the call.
- P0 counts directions from the caller: bit 0 is caller to answerer. Each
  end turns that into its own transmit and receive.
- With `+DS=...,1`, a call whose agreement falls short of `<direction>`
  ends with `NO CARRIER`.

## The codec

The codec (§§ 6 to 9):

- The dictionary is 256 trees, codewords 3 to 258 for the characters and 259
  on for strings, with the leaf recovery of § 6.5. Codewords go least
  significant bit first, from 9 bits up to N1 with STEPUP.
- The encoder starts transparent. Every 512 characters it checks what the
  window cost: it enters compressed mode if codewords would have saved a
  quarter, and goes back to transparent mode if they saved nothing (§ 7.8).
  It never sends RESET.
- When LAPM has nothing left to send, the encoder is flushed (§ 7.9), so a
  typed character or the end of a PPP frame goes out at once. An idle
  transmitter is the condition § 5.7 names for C-FLUSH.
- A decoder error (§ 5.8) ends the call.

Data that is already compressed gains nothing, and the encoder stays in
transparent mode for it.
