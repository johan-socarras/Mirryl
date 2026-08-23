# Mirryl

**Persistent wireless ADB desktop companion for Android developers and power users.**

Mirryl pairs an Android phone to a Windows PC once, then keeps them
connected over Wi-Fi automatically — reconnecting after reboots, sleep, and
network changes without the user touching a cable or a terminal again.

> This is a **showcase repository**. It documents the product's
> architecture, features, and development history — it does **not**
> contain the application's source code. See [Why no source code?](#why-no-source-code)
> below.

---

## What it does

- **One-time pairing, permanent connection.** Pair over Wi-Fi using a QR
  code (no cable required after setup) or USB. Mirryl reconnects on its own
  afterward — no repeated `adb pair` / `adb connect` commands.
- **Screen mirroring with a custom engine.** Mirryl Connect, a
  purpose-built streaming engine (not a wrapper around a third-party
  tool), handles video and audio mirroring between the phone and the
  desktop app.
- **Device management from the desktop.** A Windows companion app lists
  paired devices, shows live connection status, and exposes controls
  (restart device, rotate mirror view, lock/unlock the phone screen,
  rename devices) from one place.
- **Diagnostics built in.** Screenshot and bug-report capture straight
  from the desktop app, with local storage per device and no background
  telemetry unless the user opts in.
- **Survives Doze and reboots.** Reconnection logic was built and tested
  specifically against Android's battery-saving behavior (Doze mode), not
  just against a phone with the screen on.

## Architecture

| Layer | Technology |
|---|---|
| Desktop app | Rust + [Tauri](https://tauri.app/) (Windows) |
| Mirroring engine | Custom protocol ("Mirryl Connect"), own transport and video pipeline |
| Android companion | Kotlin/Java Android app (`dev.mirador.agent`) |
| Pairing | Wi-Fi QR pairing (Android's ADB pairing protocol) and USB |

The desktop app and the mirroring engine are separate Rust binaries that
communicate over a local socket. Video decoding on the desktop side uses
the system WebView2/Edge `VideoDecoder`, not a bundled codec.

## Project status

Actively developed, pre-release. The core flow (pairing, persistent
reconnection, mirroring, device management, diagnostics) is built and has
been validated on real hardware. Remaining work before a public release:
a licensing/activation backend, a signed installer, and finalized legal
documents (EULA, privacy policy).

## Why no source code?

This repository exists to document the project publicly — for a
portfolio, for prior-art/timestamp purposes, and so people can see what
Mirryl does without needing a build of it. The source is developed in a
private repository. A free, time-limited edition of the compiled
application will be distributed separately once the licensing system
described above is in place.

## Ownership

Mirryl is an independent product, designed, developed, and owned by
**Johan Socarras**. All rights reserved. Third-party components used
inside the compiled application are documented and attributed in the
application's own "Third-party licenses" screen, per each component's
license terms.

## Contact

Questions about Mirryl: reach out via [mirryl.com](https://mirryl.com).
