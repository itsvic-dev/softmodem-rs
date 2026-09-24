# PPP

`softmodem` does not speak PPP. It carries bytes between the line and a pty,
and `pppd` runs on the pty. There is at most one call and one `pppd`, so the
answering side runs one long-lived `pppd` on a pty that `softmodem` keeps open
for its whole life.

The ISP's modem runs with `--init ATE0Q1S0=1`: no echo, no result codes, and
auto-answer on the first ring. Without `Q1`, `RING` and `CONNECT` reach the
waiting `pppd` as line noise. Its `pppd` needs no chat script.

The caller dials and hangs up the way it would with a real modem:

```
connect    "chat -v -t 60 '' ATZ OK ATDT0300 CONNECT '\c'"
disconnect "chat -v '' '\d\d+++\d\d\c' OK ATH0 OK"
```

The caller's modem runs with `--init 'AT&C1'`, so that the end of a call
hangs up its port and `pppd` starts the next call from a clean state. See
"DCD" in [terminal](terminal.md).

A pty has no DCD, DTR or RTS/CTS, so `pppd` cannot see carrier and cannot hang
up by dropping DTR. The options that follow from that:

- `local`, since there are no modem control lines.
- `persist`, so `pppd` goes back to waiting after each call.
- `silent`, so it waits for the caller's LCP instead of sending its own into
  a line with nobody on it.
- `lcp-echo-interval` and `lcp-echo-failure`, since LCP echo is the only way
  it learns that a call has ended.
- `mru 296`, so a corrupted frame is cheap.
- `asyncmap 0`, since the path is 8-bit clean.
- `lcp-restart 15` and `ipcp-restart 15`. One LCP frame takes about a second
  each way, so a round trip is close to the 3 s default. With the default,
  each end retransmits before the answer arrives, the stale requests queue up,
  and one that arrives after LCP opens restarts the negotiation.
- `noipv6` and `noccp`, since each extra control protocol adds a second or
  more of negotiation.

VJ header compression is a trade-off on this link, not a free gain. It saves
most of the 40 byte TCP/IP header, but after a lost frame the receiver
discards compressed packets until an uncompressed one arrives, which in
practice means one TCP retransmit timeout per lost frame. Measure before
enabling it.

At 300 bit/s, 30 bytes per second, LCP and IPCP exchange a few hundred bytes
in total.
