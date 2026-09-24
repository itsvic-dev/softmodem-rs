# V.8 bis

V.8 bis (11/2000) comes before V.8, and only the answering modem may start
it at the answer. The figures:

- A signal is a dual tone for 400 ms, 285 ms for CRe and MRe, then a single
  tone for 100 ms that names it. The answering modem's pair is
  1375 + 2002 Hz, the reply's 1529 + 2225 Hz. CRe ends in 400 Hz, CRd in
  1900 Hz, ESr in 1650 Hz (§ 7.1, tables 1 and 2). CRe goes 12 to 15 dB
  below continuous signals (§ 7.1.4); this modem sends it at -26 dBm0.
- Messages go in V.21 at 300 bit/s: channel 1 from the answering modem,
  channel 2 from the caller. Each is 100 ms of mark, two flags, an HDLC
  frame with the 16-bit FCS, and a closing flag (§ 7.2). The mark after ESr
  counts as the preamble, so the demodulator here turns carrier on after
  20 ms rather than V.21's 400 ms.
- The information field is the message type and revision 2 in one octet,
  then parameter trees whose blocks end on bit 8 or bit 7 (§ 8.2.3). This
  modem sends and reads the V.8 and "transmit ACK(1)" bits of the
  identification field, and the data mode with V.22bis, V.22 and V.21 under
  it (tables 5-1, 6-2a, 6-3c). It skips all other parameters.
- The station that receives MS becomes the answering modem (§ 9.9). With
  the V.8 bit, it sends ANSam and V.8 follows as above, CM and JM taking
  priority over MS. With neither V.8 bit, it sends ANS and the modulation's
  own start-up follows.
- A station that has waited 5 s in a transaction gives up (§ 9.8), and an
  invalid frame gets NAK(1).

## Transactions

The answering modem starts with CRe, 400 ms after the answer (§ 10.2.2),
and takes whichever transaction the caller answers with:

- CRd: it sends CL, the caller sends MS, it sends ACK(1). This is
  transaction 12.
- ESr then CL, or ESr then CLR: it sends MS, or CL and MS, and the caller
  sends ACK(1). The caller has then received MS, so it sends ANSam and this
  modem goes on as the V.8 caller (transactions 2 and 3).

The calling modem answers CRe or MRe with CRd once the signal has ended,
reads CL, and sends MS with the data mode and the V.8 bit as CL has it.

spandsp has no V.8 bis, so V.8 bis is checked only between two instances of
this modem.
