# softmodem for Android

An app that makes a rooted phone a modem on its own voice line. It dials a
modem, such as a BBS, over an ordinary cellular call and shows a terminal. It
can also be a USB modem for a computer on its cable, which then dials with AT
commands, for example to run PPP.

## Requirements

- A phone with a MediaTek chipset, rooted with Magisk. It was made on an AGM
  M7 (MT6739, Android 8.1, 32-bit ARM).
- A SIM that can make voice calls. VoLTE gives the best audio, then 3G, then
  2G.

The app needs root for two things that Android keeps from ordinary apps:
recording the far end of a call, and playing sound into the call towards the
far end.

## How it works

```
terminal UI ── TCP ──────┐
                         ├── softmodem wire ── UDP wire ── bridge (root) ── cellular call
computer ── USB ACM ─────┘
```

- `softmodem` is the modem from this repository, built for Android and
  carried in the APK as `libsoftmodem.so`. Its phone line is the UDP wire.
  For the app's terminal it runs as the app, with its serial port on a
  loopback TCP port. For a computer on USB it runs as root, on the USB
  gadget's serial port (see below).
- The bridge runs as root through `app_process`. It takes the wire's calls
  and places them as cellular calls:
  - It dials with Telecom, so the digits after a comma are keyed out of band
    once the call is answered. A menu behind a carrier trunk takes digits
    only this way.
  - It sends the modem's audio into the call through MediaTek's BGS path
    (`Set_BGS_UL_Mute=0`), and mutes the microphone.
  - It records the far end with the `VOICE_DOWNLINK` source, and holds its
    level with a slow AGC.
  - For the call, it turns off the downlink noise reduction and expander of
    MediaTek's speech processing, which take FSK for noise. It writes a copy
    of `/vendor/etc/audio_param/Speech_AudioParam.xml` with both off to
    `/data/vendor/audiohal/audio_param/`, and removes it after the call, so
    ordinary calls keep them.
  - It tells the modem the call is answered only when the call is active and
    the digits are keyed, so the modem does not listen to a menu's prompt.
- Closing the TCP connection hangs up, as DTR dropping would.

## What works

Mobile voice codecs (AMR, EFR) keep speech, not modem signals. Over VoLTE
they also lose whole voice frames, which damages 40 to 180 ms of the signal
a few times a minute. Measured over Orange in Poland, with the fixed
modulation and automode off:

| Modulation | Result |
|---|---|
| V.21, 300 bit/s | Holds a call and carries text. With PPP, about one frame in four fails its check |
| V.22, 1200 bit/s, with V.42 | Holds a call. LAPM resends what the codec damages, so PPP loses no frames, and carries 7 to 13 kB a minute |
| V.22, 1200 bit/s, without V.42 | As V.21, with about one PPP frame in four lost, but four times as fast |
| V.22bis, 2400 bit/s | Connects, then retrains and drops. Too many errors for PPP |
| V.34, V.90 | Do not finish training |

V.22 with V.42 is the best choice for a far end that has V.42, as most
modems do. V.21 is the default, for any far end. The app warns when you
choose another modulation.

The phone's calls use AMR-NB at 12.2 kbit/s, and the codec sets the limit.
It passes a V.22 signal with a signal to noise ratio of about 14 dB,
whatever the level. At 1200 bit/s, with four points, the equalised receiver
then makes almost no errors, and what LAPM resends comes from the radio. At
2400 bit/s, with 16 points, about 2% of the symbols arrive wrong on a clean
radio link, and 5 to 9% on a real one, so neither direction holds V.22bis
at 2400 bit/s. The 1800 Hz guard tone that the answering end adds doubles
those errors. V.34, even at 2400 baud, keeps only about 7 dB through the
codec. How many characters arrive wrong changes from call to call, with the
radio. The codec also fades the carrier out for a second at times, so the
app sets `S10=50` and waits 5 seconds before it takes a lost carrier as the
end of the call.

A call through the phone has a round trip of about 1.7 s: the network, and
the bridge's audio buffers. The caller waits a round trip longer for the
answer to V.42, and at V.22 it also shortens its wait before data. V.42 then
comes up over a round trip of up to about 1.9 s at V.22, and 2 s at
V.22bis. Over a longer round trip, V.42 misses the answerer's detection
phase and the call goes on without it.

At times the far end's automode does not hear the caller's V.22 and falls
back to V.21, which a caller fixed on V.22 cannot follow, so the call does
not connect. Dial again.

## Building

You need:

- The Android SDK, with `ANDROID_HOME` set, or `sdk.dir` in
  `local.properties`.
- The NDK version in `app/build.gradle.kts`:
  `sdkmanager --install 'ndk;30.0.16248370'`.
- `rustup` with the stable toolchain and the Android target:
  `rustup target add --toolchain stable armv7-linux-androideabi`. The Nix dev
  shell's Rust has no Android target.

Then:

```
cd android
./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

The `cargoBuild` task builds `softmodem` with the NDK's clang as the linker,
and puts it in the APK.

## Using it

- Type the number. Each comma waits 3 seconds, and the digits after the
  first comma are keyed once the call is answered, for a menu:
  `0300,,,1234#`.
- Choose the highest modulation: V.21, V.22, V.22bis, V.34 or V.90. V.21 is
  the default. The others are there to try, as the codec is not expected to
  carry them.
- Automode lets the modem fall back from that modulation to slower ones. It
  is off by default.
- Dial with the button or the green call key.
- In the terminal, type a line and send it with the OK key or Send.
- Back, or Hang up, ends the call.

The first call asks Magisk for root.

## A computer on USB

"Serve a computer on USB" makes the phone a USB modem for a computer on its
cable. The computer dials with AT commands, and the app's terminal is off
until you stop it, in the app or in the notification. It keeps running when
you leave the app.

- The modem serves the port with `--serial /dev/ttyGS0`, the ACM function
  of the phone's USB gadget. MediaTek's USB configurations with adb and with
  MTP carry it (`sys.usb.acm_enable=1`). Only root can open the device, so
  in this mode the modem runs as root too.
- It takes the modulation and automode chosen on the dial screen. Its
  stored profile is `S10=50` and both in `+MS`, such as `+MS=V21,0`, so
  `ATZ` keeps them. The computer can change them with `AT+MS`.
- The gadget has no control lines. DCD and RI are not signalled, and the
  computer dropping DTR does not hang up. Hang up with `+++` and `ATH`.
  Unplugging the cable hangs up.
- The service holds a wake lock, so that the modem keeps time with the
  screen off.

On Linux, the phone is `/dev/ttyACM0`, USB ID `0e8d:2006`. ModemManager
probes new ACM ports with its own AT commands, so tell it to leave the phone
alone, in `/etc/udev/rules.d/99-softmodem.rules`:

```
ATTRS{idVendor}=="0e8d", ATTRS{idProduct}=="2006", ENV{ID_MM_DEVICE_IGNORE}="1"
```

Then, with hardware flow control off:

```
minicom -D /dev/ttyACM0
```

On macOS the phone is `/dev/cu.usbmodem*`.

### PPP

PPP runs over V.22 with V.42, but pppd's default timers are shorter than the
round trip, and a loss burst makes LAPM resend for a second or more. A late
duplicate request can then reopen LCP after it has opened. Give the
computer's pppd long timers, and no echo requests:

```
lcp-restart 15
ipcp-restart 15
pap-restart 15
lcp-echo-interval 0
asyncmap 0
mru 296
mtu 296
```

LCP and PAP then finish in 12 to 18 s after `CONNECT`.

## Limits

- The bridge depends on MediaTek's audio HAL. Other chipsets need another
  way into the call's audio.
- The terminal shows text only. ANSI colours and cursor moves are dropped.
- The app must stay open during a call from its terminal. It keeps the
  screen on, and comes back to the front when the system's in-call screen
  covers it.
- Debug builds record each call with `--dump` in the app's files, for
  `adb exec-out run-as dev.itsvic.softmodem cat files/dumps/<file>`.
