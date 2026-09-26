# softmodem for Android

An app that makes a rooted phone a modem on its own voice line. It dials a
modem, such as a BBS, over an ordinary cellular call and shows a terminal. No
computer is needed.

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
terminal UI ── TCP ── softmodem wire ── UDP wire ── bridge (root) ── cellular call
```

- `softmodem` is the modem from this repository, built for Android and
  carried in the APK as `libsoftmodem.so`. It runs as the app, with its
  serial port on a loopback TCP port and its phone line on the UDP wire.
- The bridge runs as root through `app_process`. It takes the wire's calls
  and places them as cellular calls:
  - It dials with Telecom, so the digits after a comma are keyed out of band
    once the call is answered. A menu behind a carrier trunk takes digits
    only this way.
  - It sends the modem's audio into the call through MediaTek's BGS path
    (`Set_BGS_UL_Mute=0`), and mutes the microphone.
  - It records the far end with the `VOICE_DOWNLINK` source.
  - It tells the modem the call is answered only when the call is active and
    the digits are keyed, so the modem does not listen to a menu's prompt.
- Closing the TCP connection hangs up, as DTR dropping would.

## What works

Mobile voice codecs (AMR, EFR) keep speech, not modem signals. Measured over
Orange in Poland:

| Modulation | Result |
|---|---|
| V.21, 300 bit/s | Holds a call, with an odd wrong character |
| V.22, 1200 bit/s | Connects, then bursts of errors from the codec, in both directions |
| V.22bis and faster | Not expected to work |

An equalizer in the receiver does not help, because the codec damages whole
frames. So the app defaults to V.21, and warns when you choose another
modulation. How many characters arrive wrong changes from call to call, with
the radio. The codec also fades the carrier out for a second at times, so
the app sets `S10=50` and waits 5 seconds before it takes a lost carrier as
the end of the call.

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
- Choose the modulation. V.21 is the default.
- Dial with the button or the green call key.
- In the terminal, type a line and send it with the OK key or Send.
- Back, or Hang up, ends the call.

The first call asks Magisk for root.

## Limits

- The bridge depends on MediaTek's audio HAL. Other chipsets need another
  way into the call's audio.
- The terminal shows text only. ANSI colours and cursor moves are dropped.
- The app must stay open during a call. It keeps the screen on, and comes
  back to the front when the system's in-call screen covers it.
- Debug builds record each call with `--dump` in the app's files, for
  `adb exec-out run-as dev.itsvic.softmodem cat files/dumps/<file>`.
