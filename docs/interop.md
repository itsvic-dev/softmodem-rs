# Interop

## Real modems

The real modem at hand is a generic serial 56k modem, reported as a standard
AT modem by both Windows and Slackware. That says nothing about the chipset.
`ATI3`, `ATI4` and `ATI6` usually identify it. Which chipset it is, and
whether it reaches V.21 from automode, are open questions.

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

## spandsp

`softmodem-interop` checks the modem against spandsp, whose V.21 and answer
tones are in many real products. It links spandsp, so it only builds in the
dev shell, and the modem itself does not depend on it. spandsp is a black
box: see "Rejected" in [scope](scope.md).

There are two versions to test against, and one test binary can link only
one of them:

- The 3.0.0 snapshot from 2020 that nixpkgs ships, in the default dev shell.
- 3.1.1, the first tagged release, in `nix develop .#spandsp-3_1`, built with
  its V.34 modem. `cfg(spandsp_3_1)` selects the parts of the binding that
  changed. Its XID leaves out the value of the HDLC optional functions but
  still counts it in the group length, so a strict parser refuses the frame.
  spandsp's own parser accepts it. The package patches this.

What spandsp does that matters here:

- Its V.22bis modem started at 1200 bit/s is the V.22 reference. Its answer
  tones are a separate part.
- It has no V.8 bis and does not answer CRe, so an answering modem that
  meets it goes on to ANSam at 2 s.
- Its V.42 sends only zeros until `v42_restart`, although its header
  declares a `v42_start` that the library does not export.
- Its V.42 acknowledges every I frame with an RR response with the F bit
  set. See [V.42](v42.md).
- It proposes and accepts V.42bis only from caller to answerer, with 512
  codewords and strings of 6. This modem agrees to that in both roles.
- It sends data at once after `CONNECT`, as `pppd` does. See
  [V.25](v25.md).

What spandsp cannot stand in for is a modem's automode, the probing a real
modem does when it hears ANS instead of ANSam. That needs the real modem.

## Known cost

Against a far end without V.42, the link is raw async: PPP drops frames that
fail FCS and TCP retransmits, which is correct but wasteful. Against one
without V.42bis, or one like spandsp that compresses in one direction only,
text gives up the factor of two or three that compression provides.
