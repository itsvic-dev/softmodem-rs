# Interop

## Real modems

The first real modem is a generic serial 56k modem, reported as a standard
AT modem by both Windows and Slackware. That says nothing about the chipset.
`ATI3`, `ATI4` and `ATI6` usually identify it.

A modern modem calls with V.8 and expects ANSam, a 2100 Hz tone with 15 Hz
amplitude modulation. In automode this modem sends ANSam and offers V.22bis
and V.21 in JM, so a V.8 caller should meet it at 2400 bit/s. A V.8 caller
that also offers V.32bis or V.34 will see only the modes both have. Should a
chipset still refuse, force it on the calling modem with a chipset-specific
command (for example `AT+MS=V22B` or `AT+MS=V21` on Rockwell parts), and
dial blind with `ATX3`.

A V.90 or V.92 modem that answers may start with V.8 bis before ANSam, and
V.8 bis lets it clear the call when nothing answers (§ 10.2.2). In automode
this modem answers CRe as the calling modem, and sends CRe itself when it
answers, so it meets such a modem on either side of the call.

## Against spandsp

`softmodem-interop` checks the modem against spandsp, whose V.21 and answer
tones are in many real products. It links spandsp, so it only builds in the
dev shell, and the modem itself does not depend on it.

- spandsp demodulates our V.21, and we demodulate spandsp's, on both
  channels.
- spandsp's detector hears our answer tone as V.25 ANS.
- Our V.22 pump trains with spandsp's V.22 and carries data both ways,
  as caller and as answerer, with and without the guard tone. spandsp's
  V.22bis falls back to it at 1200 bit/s.
- Our V.22bis pump trains with spandsp's at 2400 bit/s as caller and as
  answerer, and falls back to 1200 bit/s when spandsp is held there.
- spandsp hears our ANSam with and without phase reversals, and we tell its
  four answer tones apart.
- Our automode negotiates V.8 with spandsp's as answerer and as caller,
  agrees on V.22bis, and then trains at 2400 bit/s with spandsp's V.22bis.
  With a spandsp that offers only V.21, V.8 agrees on V.21. spandsp has no
  V.8 bis and does not answer CRe, so these calls also show the answering
  modem going on to ANSam at 2 s.
- Whole calls in automode: a V.25 caller that answers our USB1 as V.21, and
  an answerer that sends ANSam but speaks only V.21, both reach V.21. Each
  hears some of the other modulation first as noise, which is why those two
  tests allow junk before the data.
- Whole calls, with spandsp's parts as the far modem: we call one that
  answers with ANS, with ANS and phase reversals, and with V.8 ANSam and
  phase reversals, the tone a modern modem sends. A V.25 caller that waits for
  our answer tone and channel 2 calls us. Data crosses both ways each time.
  spandsp's side is plain start-stop there, so our V.42 falls back.
- Our V.42 against spandsp's, bit for bit with no modulation under them:
  with the detection phase and straight into LAPM, as caller and as
  answerer, 3000 octets each way. With one bit in 10007 flipped each way,
  REJ and timer recovery still deliver all of it. spandsp's V.42 sends only
  zeros until `v42_restart`, although its header declares a `v42_start`
  that the library does not export.
- Our V.42bis codec against spandsp's: each decodes what the other encodes,
  with 512, 2048 and 4096 codewords, and with spandsp's encoder dynamic,
  always compressed and never compressed, over text, noise and runs.
- V.42bis negotiated with spandsp's V.42, and its codec run on the direction
  agreed. spandsp proposes and accepts compression only from caller to
  answerer, with 512 codewords and strings of 6, and this modem agrees to
  that in both roles.

Those calls found a bug no test between two of our own modems could: the
answering modem reports `CONNECT` up to a second before the caller does, and
spandsp, like `pppd`, sends at once. The caller dropped what arrived before
its own `CONNECT` and misframed the first characters. It now keeps them.

What spandsp cannot stand in for is a modem's automode, the probing a real
modem does when it hears ANS instead of ANSam. That still needs the real
modem.

## Known cost

Against a far end without V.42, the link is raw async: PPP drops frames that
fail FCS and TCP retransmits, which is correct but wasteful. Against one
without V.42bis, or one like spandsp that compresses in one direction only,
text gives up the factor of two or three that compression provides.
