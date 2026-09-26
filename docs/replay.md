# Replay

`softmodem replay` plays a recorded call again into a fresh modem and logs
what the modem does, on the call's own clock. Use it to find out why a
call failed without placing the call again.

## What `--dump` records

For each call, `--dump DIR` writes three files with one prefix,
`DIR/<seconds>-<role>`:

- `-rx.wav`: the samples received, after loss and gap fill.
- `-tx.wav`: the samples sent.
- `.journal`: all the line was given. The version, the offer, the V.42
  setup of each role and the role come first. Then there is one event on
  each line, after the nanoseconds since the call started: each frame
  received and made, a frame made but not sent, the bytes from the
  computer, and each retrain and cleardown.

The two WAV files do not show how the modem interleaved sending and
hearing, or what the computer sent. The journal records both. Thus a
replay gives the line the same inputs in the same order and at the same
times.

## Replaying a call

```
softmodem replay dumps/1790419150-answer
```

The prefix can also be the path of one of the three files. The log goes
to stdout, with the seconds into the call first:

```
    5.020  INFO {end="near"}: handshake stage="JM with V.90 digital, V.34, V.22bis, V.21"
    5.702  INFO {end="near"}: handshake stage="V.90 digital phase 2 INFO0"
   12.901  INFO {end="near"}: carrier on
   13.260  INFO {end="near"}: CONNECT 56000, error control LAPM, compression V42B
   30.200  INFO {end="near"}: cleared down
   30.200  INFO {end="near"}: sent all as recorded frames=1511
```

Each stage is what `DataPump::stage` says the pump sends and listens for.
The live modem logs the same stages. `RUST_LOG=debug` also logs the bytes
to and from the computer.

The pumps and LAPM have no randomness and take time only as an argument.
Thus, with the same code, a replay sends exactly what the call sent. The
replay compares each frame it sends with `-tx.wav`. If a frame is
different, it gives the first sample that is different:

```
   25.080  WARN {end="near"}: sends other samples than the recording from here sample=200672 recorded=1895 replayed=1903
```

This occurs when the code changed after the recording. It also occurs
when an input that is not in the journal changed the call. After a fix,
the replay of a failed call shows whether the fix changes the call, and
from which point.

## The far end

`--far` also plays what this end sent into a second modem, in the far
end's role, with the same settings. Its log lines have `end="far"`. It
shows what a modem like this one hears in the signal that this end sent,
for example when it sees the CM, or if it can train on our TRN.

The far model hears only this end. Nothing hears what the far model sends.
Thus, after the handshake, its LAPM gets no replies, and after a time it
can end its side of the call. Do not use its log after that point.

## Recordings without a journal

A recording from another source, such as a stereo file from the
slmodemd checks, or an old dump, has no journal:

```
softmodem replay --rx call.wav --rx-channel 1 --tx call.wav --tx-channel 0 \
  --role answer --init 'AT+MS=V90'
```

The file must be 16-bit samples at 8 kHz. Use `sox -r 8000 -b 16 -e
signed` to change it. `--role` and `--init` give what the journal
otherwise gives. The modem sends a frame and then hears one, every 20 ms.
This is not always the order of the real call, and there is no data from
the computer. Thus the comparison with `--tx` is correct only up to the
first point where the order or the data had an effect.
