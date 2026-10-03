# Front panel

`--panel` shows the modem as an external modem on a desk, with the row of
lights of a US Robotics Courier, and a strip of embossing tape under it
that says what the call is doing. Each process
has its own window, titled with its serial port. Closing the window hangs up
and stops the modem, as Ctrl-C does, and the window closes when the modem
stops.

```
softmodem wire --local 127.0.0.1:5301 --peer 127.0.0.1:5300 --tcp 127.0.0.1:2323 --panel
```

The window is in `softmodem-panel`, drawn with gpui, Zed's UI library. It is
behind the `panel` feature of `softmodem`, off by default, so that the tests,
CI and the VM checks do not build gpui:

```
nix develop --command cargo run -p softmodem --features panel -- wire ... --panel
```

So far it is built and tried on macOS only.

## Lights

| Light | Lit when |
|---|---|
| HS | Connected at 9600 bit/s or faster. |
| AA | `S0` is not 0. It blinks while a call rings. |
| CD | Connected, in data or online command mode. |
| OH | Off hook: dialling, ringing out, in a call, or after `ATH1`. |
| RD | Flashes as data goes from the line to the computer. |
| SD | Flashes as data goes from the computer to the line. |
| TR | A computer is on the serial port. Only the TCP port knows; the others keep it lit. |
| MR | Always, as the power light. It blinks while the modems train. |
| RS, CS | Always, as the modem has no flow control lines. |
| SYN | Never, as the modem has no synchronous mode. |
| ARQ | LAPM corrects errors. It goes dark for a moment when an I frame is sent again. |

RD and SD flash once for each run of data, with a short dark gap after each
flash, so that a steady stream flickers as on the real modem rather than
holding the light on.

## Text

Before a call, the text says whether the modem is on hook, off hook, ringing,
or waiting for a computer. While the modems train it gives the handshake
stage, as the debug log names it. Once connected it gives the rates, with
both where they differ as in V.90, the error control and compression, the
time on line, the bytes each way, and the I frames sent again, if any.

## How it is joined to the modem

The modem publishes a `Status` on a `tokio::sync::watch` channel each time
round its loop, if anything changed. The window reads it 60 times a second
and fades each light towards what the status says, so that it does not
depend on how often the modem publishes.

gpui must own the main thread on macOS, so with `--panel` the tokio runtime
runs on a second thread. The desk, the case and its printing are one SVG,
drawn at twice its size so that it stays sharp on a Retina screen. Its
textures are SVG filters: lit noise for the moulded plastic, and stretched,
displaced noise for the wood. It is drawn once, before the window opens,
because an image that gpui loads by itself appears only at the next redraw,
and an idle panel does not redraw. The lights and the tape are drawn over
it.
