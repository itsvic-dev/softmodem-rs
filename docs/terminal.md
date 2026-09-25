# Terminal

`openpty`, 8N1 async framing, and an AT interpreter.

The computer talks to the modem, never to the line, in command mode. It
controls the modem with the basic Hayes command set, and the modem places and
takes calls through its transport as a result. One process both dials and
answers, as one modem on one line does.

## Modes

- **Command mode, on hook.** Bytes from the computer are command lines.
  An incoming call gives `RING`, repeated every 6 s while the far end waits.
- **Dialling and handshake.** After `ATD` or `ATA`. Any byte from the
  computer aborts with `NO CARRIER`, and so does no `CONNECT` within `S7`.
- **Data mode.** Bytes pass to and from the line. `+++` with guard times
  goes to online command mode. `RUST_LOG=softmodem=trace` logs them in
  hex, which shows what the computer sends to log in, password included.
- **Online command mode.** The call stays up, the line idles on mark. `ATO`
  goes back to data mode, `ATH` hangs up.

The far end hanging up, or carrier lost for `S10`, gives `NO CARRIER` and
command mode, on hook.

## Commands

| Command | Effect here |
|---|---|
| `A` | Answer a ringing call. |
| `A/` | Repeat the last command line, at once, without `AT` or CR. |
| `D` | Dial. See the modifiers below. |
| `E0`, `E1` | Command echo off, on. |
| `H0`, `H1` | On hook (hang up), off hook (incoming calls get busy). |
| `I0` to `I9` | Identification. `I0` product, `I3` version, `I4` modulations. |
| `L0` to `L3` | Speaker volume. `L0` and `L1` low, `L2` medium, `L3` high. |
| `M0` to `M2` | Speaker off, on until `CONNECT`, always on. |
| `O`, `O1` | Return to data mode from online command mode. `O1` retrains a V.22bis line first. |
| `Q0`, `Q1` | Result codes shown, suppressed. |
| `V0`, `V1` | Result codes as digits, as words. |
| `X0` to `X4` | Which result codes are used, see below. |
| `Z` | Hang up and reset to the stored profile. |
| `Sn=v`, `Sn?` | Write, read an S-register. |
| `+MS=<carrier>[,<automode>[,<rates>...]]` | The highest modulation for the next call, `V21`, `V22`, `V22B`, `V34` or `V90`, whether automode may fall back from it, and the rates, of which the fastest to transmit at counts (see below). The default is `V90` with automode. With `V90` the caller is the analogue modem and the answerer the digital modem. |
| `+MS?`, `+MS=?` | Read the modulation, list those supported. |
| `+ES=<orig_rqst>[,<orig_fbk>[,<ans_fbk>]]` | How to try V.42, as V.250 § 6.5.1. The default is `3,0,2`: try it with the detection phase, and fall back to plain data. |
| `+ES?`, `+ES=?` | Read the error control, list the values supported. |
| `+ER=0`, `+ER=1` | Report the error control in use before `CONNECT`, off, on. |
| `\N0` to `\N3` | Set all of `+ES`: `\N0` and `\N1` no V.42 (`1,0,1`), `\N2` V.42 required (`3,3,5`), `\N3` V.42 if the far end has it (`3,0,2`). |
| `+DS=<direction>[,<required>[,<max_dict>[,<max_string>]]]` | How to ask for V.42bis, as V.250 § 6.6.1. `<direction>` is `0` none, `1` transmit only, `2` receive only, `3` both. `<required>` `1` hangs up unless the far end agrees to all of it. The default is `3,0,2048,32`. |
| `+DS?`, `+DS=?` | Read the compression, list the values supported. |
| `+DR=0`, `+DR=1` | Report the compression in use before `CONNECT`, off, on. |
| `%C0` to `%C3` | `%C0` no V.42bis (`+DS=0`), `%C1` to `%C3` both directions (`+DS=3`). |

`+ES` takes these values:

- `<orig_rqst>`, when the modem dials: `0` or `1` no V.42, `2` V.42 without
  the detection phase, `3` V.42 with it. `4`, the deleted alternative
  protocol, gives `ERROR`.
- `<orig_fbk>`, when the modem dials: `0` or `1` fall back to plain data,
  `2` or `3` hang up without V.42.
- `<ans_fbk>`, when the modem answers: `0` or `1` no V.42, `2` or `3` V.42
  if the far end has it, `4` or `5` hang up without V.42.

V.250 tells direct and buffered operation apart. On a pseudoterminal there is
no DTE rate to match, so they are the same here. A subparameter left out
keeps its value.

`+MS` takes the V.250 form `+MS=<carrier>[,<automode>[,<min_tx_rate>[,<max_tx_rate>[,<min_rx_rate>[,<max_rx_rate>]]]]]`.
`<max_tx_rate>` in bit/s, other than 0, is the fastest this modem transmits
at. For now only the V.90 analogue modem keeps to it: it enables no faster
upstream rate, so the digital modem cannot ask for one, as in
`AT+MS=V90,1,,24000`. The other rates are accepted and ignored. When the two
directions differ, the log gives the transmit rate after CONNECT's. As
V.250 § 6.4.2 has it, `+MS=<carrier>` on
its own turns automode on again, so a fixed modulation needs `,0`:
`AT+MS=V21,0`. Without automode, V.22bis still falls back to V.22, which is
part of V.22bis itself. As V.250 requires, a basic command after `+MS` on the
same line needs a `;` first: `AT+MS=V22,0;E0`.

Other extended commands (`&`, `\`, `%` and `+` prefixes) are accepted and
return `OK` without effect, because chat scripts send chipset-specific
strings such as `AT&C1&D2` and fail on `ERROR`.

The stored profile is the Hayes defaults with the `--init` command line
applied, for example `--init 'ATS0=1'` on the ISP. It is applied at start
and again by every `ATZ`. So the ISP's `pppd` needs no chat script.

## Dial modifiers

A VoIP call sends the whole number at once, so most modifiers have no
meaning. `T` and `P` are ignored. `W` and `@` return at once, as there is
always a dial tone and never a wait for quiet. `!` is ignored. `,` waits `S8`
seconds before the call is placed. These work as on a real modem:

- `R` dials in reverse mode: the modem answers the call it placed, with
  answer tone and channel 2.
- `;` places the call and returns to command mode without a handshake.
- `L` redials the last number.

What remains is digits, `*`, `#` and `A` to `D`, which become the number
given to the transport. For SIP that becomes a URI under dialling rules that
are configuration, not code.

## Result codes

| Digit | Words | From |
|---|---|---|
| 0 | `OK` | `X0` |
| 1 | `CONNECT` | `X0` |
| 2 | `RING` | `X0` |
| 3 | `NO CARRIER` | `X0` |
| 4 | `ERROR` | `X0` |
| 5 | `CONNECT 1200` | `X1` |
| 6 | `NO DIALTONE` | `X2`, `X4` |
| 7 | `BUSY` | `X3`, `X4` |
| 10 | `CONNECT 2400` | `X1` |

At 300 bit/s Hayes reports plain `CONNECT`. Under `X0`, `CONNECT 1200` and
`CONNECT 2400` are plain `CONNECT` too. Below the level that has them,
`BUSY` and `NO DIALTONE` become `NO CARRIER`. The default is `X4`. `NO DIALTONE` never happens. With `V1` each code is
framed by CR LF, with `V0` it is the number and CR, both using `S3` and `S4`.

Under `+DR=1`, `+DR: V42B`, `+DR: V42B RD`, `+DR: V42B TD` or `+DR: NONE`
follows, as V.250 § 6.6.3 has it: both directions, receive only, transmit
only, or none.

Under `+ER=1`, `+ER: LAPM` or `+ER: NONE` comes on its own line before
`CONNECT`, in words under `V0` too, as V.250 § 6.5.5 has it. `CONNECT`
itself does not change with V.42, so chat scripts that wait for
`CONNECT 2400` still work.

## S-registers

All 256 hold a value. These have an effect:

| Register | Meaning | Default |
|---|---|---|
| `S0` | Rings before auto-answer, 0 for never. | 0 |
| `S1` | Rings counted so far. | 0 |
| `S2` | Escape character. | 43, `+` |
| `S3` | Line terminator. | 13, CR |
| `S4` | Response line feed. | 10, LF |
| `S5` | Backspace. | 8 |
| `S6` | Seconds to wait before a blind dial, with `X0`, `X1`, `X3`. | 2 |
| `S7` | Seconds to wait for `CONNECT` after `D` or `A`. | 50 |
| `S8` | Seconds per `,` in a dial string. | 2 |
| `S10` | Tenths of a second without carrier before hanging up. | 14 |
| `S12` | Escape guard time, in fiftieths of a second. | 50 |

`S9`, carrier detect time, is stored but not used: V.21 sets it to 300 to
700 ms, and its demodulator keeps 400 ms. V.22 sets 105 to 205 ms, and its
demodulator keeps 150 ms.

## DCD

`&C0`, the Hayes default, keeps DCD on. `&C1` makes it follow carrier: on at
`CONNECT`, off when the call ends, 200 ms after the result code so that the
computer can read it first.

A pseudoterminal has no DCD line, so dropping carrier hangs up the port
instead. The modem opens a fresh pseudoterminal, moves the link to it, and
closes the old one. Every program that had the old one open sees a hang-up,
which `pppd` reports as `Modem hangup`, and one that reopens the link gets the
new port. `pppd` notices at its next write, so within its LCP echo interval.
Rising DCD cannot be shown this way, and a terminal program such as `minicom`
has to reopen the port after each call, so `&C1` is for `pppd`.

What emulators see, as of 86Box's `char` layer and QEMU's 16550:

- 86Box's pipe backend, pointed at the pseudoterminal, reports DCD as "the
  descriptor is open". The hang-up drops it until the backend reconnects,
  which it does only with reconnect enabled. The guest sees DCD fall at the
  end of a call and nothing else.
- 86Box's host serial backend and QEMU's `-chardev serial` read DCD and RI
  with `TIOCMGET`, which a pseudoterminal does not answer.

`--cuse NAME` serves the port as `/dev/NAME` through CUSE instead, a
character device answered from user space, on Linux only. It answers the
termios ioctls, including `TCGETS2`, `TIOCMGET` with DCD, RI, DSR and CTS,
and `TIOCMSET`, `TIOCMBIS` and `TIOCMBIC`, which store DTR and RTS. RI is on
for 2 s of each 6 s ring.

It needs the `cuse` module and access to `/dev/cuse`, and the node it
creates is root's, mode 0600, unless a udev rule says otherwise. `pppd`
cannot use it, since it is not a kernel tty and so takes no line
discipline.

`--tcp ADDR` listens on `ADDR` and serves one connection at a time. A
second waits in the listen queue until the first closes. The end of a
connection is DTR dropping: the modem hangs up, goes back to command mode
and waits for the next one, with its settings kept. While none is open,
the modem's output is lost, and an incoming call still rings and, with
`S0` set, is answered. Writes are sent at once, with Nagle's algorithm off.

TCP has no control lines, and so QEMU's 16550 and its `usb-serial` keep
the reset value of their modem status, DCD, DSR and CTS on, as a modem at
`&C0` shows. `-chardev serial` on a pseudoterminal reads all of them as
off instead, since QEMU ignores the failed `TIOCMGET`. Windows' modem driver
turns hardware flow control on by default, and a guest with it on does not
send while CTS is off, so a Windows guest needs a TCP or pipe backend. The `&C1` in Windows' init
string does nothing on a TCP port.

Which port for what:

| Computer | Port |
|---|---|
| QEMU, on Linux | `--cuse`, with `-chardev serial` |
| QEMU or UTM, elsewhere | `--tcp`, with a TCP client backend, telnet off |
| 86Box, on Linux | `--cuse`, with the host serial backend |
| 86Box, elsewhere | `--pty`, with the pipe backend, reconnect on |
| `pppd` on the host | `--pty` |

## Not modelled

A pseudoterminal has no DTR, so the computer cannot hang up by dropping it.
`pppd` hangs up with `+++` and `ATH` in its disconnect script, as on a line
without modem control. RI is not signalled either.

The pty is the DTE side and is much faster than the line. `softmodem` keeps a
small transmit buffer and stops reading from the pty when it is full. The
kernel pty buffer then fills and the writer blocks, which is the only
backpressure a pty offers. An unbounded buffer here turns into minutes of
queued latency at 300 bit/s.
