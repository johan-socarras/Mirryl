# Mirryl low-latency session protocol

## Scope

This document defines Mirryl's independently designed Android-to-Windows media
transport and control messages. It does not claim compatibility with any other
application or reuse another application's wire layout.

Every mirror is an isolated session. A session owns:

- one Android serial;
- one random HTTP token and loopback port;
- one unique temporary Android agent path;
- one video encoder process;
- one audio capture process;
- one persistent input-control process; and
- one Mirryl Connect window.

No global device, media queue, control stream, or Android temporary path may be
shared between sessions. Multiple sessions may run concurrently.

## Framed video stream

The Android video agent writes a sequence of 24-byte headers followed by one
complete MediaCodec output access unit.

All multibyte integers use network byte order.

| Offset | Size | Field |
| --- | ---: | --- |
| 0 | 4 | ASCII magic `MYV1` |
| 4 | 1 | Protocol version, currently `1` |
| 5 | 1 | Flags: bit 0 configuration, bit 1 keyframe |
| 6 | 2 | Header size, currently `24` |
| 8 | 4 | Payload size |
| 12 | 4 | Monotonic packet sequence |
| 16 | 8 | Android MediaCodec presentation time in microseconds |

The maximum access-unit payload is 16 MiB. Configuration data and media access
units remain separate packets. The Windows viewer may prepend the most recent
configuration packet to a keyframe when required by its decoder.

The host must never synthesize presentation timestamps. It uses the encoder PTS
to detect decoder backlog and to recover to the newest keyframe.

## Video recovery

The browser decoder queue has a hard upper bound. If it exceeds the bound:

1. pending decoded frames are closed;
2. the decoder is reset and configured again;
3. delta access units are discarded;
4. the host asks the Android encoder for a new keyframe; and
5. decoding resumes from that keyframe.

The session reports queue depth, PTS backlog and recovery count to a private
loopback telemetry endpoint for diagnostics.

## Persistent control stream

The Windows host starts one Android control process per session and keeps its
stdin open. Each control record is 24 bytes in network byte order.

| Offset | Size | Field |
| --- | ---: | --- |
| 0 | 4 | ASCII magic `MYC1` |
| 4 | 1 | Protocol version, currently `1` |
| 5 | 1 | Kind: `1` pointer, `2` key |
| 6 | 1 | Pointer action or zero |
| 7 | 1 | Reserved |
| 8 | 4 | X coordinate or Android keycode |
| 12 | 4 | Y coordinate or zero |
| 16 | 4 | Buttons or zero |
| 20 | 4 | Monotonic message sequence |

The Android agent injects pointer down, move, up and cancel events as they are
received. Key messages inject a down/up pair. The local Mirryl window sends
pointer events through one WebSocket for the lifetime of the page, avoiding a
new HTTP or ADB process for each movement.

## Encoder requirements

- H.264 surface input using the device hardware encoder when available.
- Real-time priority.
- Encoder queue latency of one frame on Android 8 and newer.
- No B-frames.
- Ten-second periodic keyframe interval.
- Repeated static frame after 100 ms to guarantee prompt first-frame delivery.
- Explicit keyframe request support through the video process stdin.

## Multi-device lifecycle

The interactive launcher starts a detached Mirryl child for each selected
serial and immediately returns to the device menu. “Open all” starts every
authorized device not already launched by the current menu.

Native Mirryl viewer windows use a dedicated WebView2 profile. Every window,
URL, token, stream, title and Android process remains session-specific.
