# Independent implementation boundary

> Renamed from `CLEAN_ROOM.md` on 2026-08-18. The old title was wrong: a
> clean-room process requires two separated teams — one that inspects the
> original and writes a specification, and a second, with no exposure to the
> original, that implements from that specification alone. That is not what
> happened here, and claiming it invited a contradiction that a reviewer would
> find immediately. What follows describes what was actually done.

## Objective

Build an independently authored Android mirroring and control product without
incorporating scrcpy source code, binaries, protocol definitions, assets, names,
strings, tests, or documentation.

## Reference material

The reference checkout is stored outside this project at:

```text
work/reference/scrcpy
```

Reference baseline:

- Repository: `Genymobile/scrcpy`
- Commit: `2926c06c5dc3064ae6d8db706f1a98a37cfcf3f0`
- Reported version: `4.1`
- Inspection date: `2026-07-25`

It was consulted to understand externally observable capabilities, operating
system constraints, and broad subsystem boundaries. It was a single developer
doing both the reading and the writing, so this is an independent
reimplementation, not a clean-room one, and the file should not be read as
claiming that no part of the reference was ever seen. The controls below are
what keep the result independent in substance: no source copied, no names
reused unless Android imposes them, and a wire protocol specified separately
before any code was written.

## Allowed inputs

- Official Android SDK and AOSP documentation.
- Public ADB command documentation.
- Public media-codec and Windows media documentation.
- Observed behavior from a legitimately obtained build on an authorized device.
- High-level architectural facts that are necessary for interoperability.

## Prohibited inputs

- Copying or translating source code from scrcpy.
- Reusing its class, function, field, constant, protocol, or test names when the
  name is not imposed by Android or another public standard.
- Reusing wire layouts, magic values, command-line syntax, assets, icons,
  documentation, help text, or error messages.
- Shipping any scrcpy executable, server artifact, or derived artifact.
- Claiming compatibility with, endorsement by, or affiliation with scrcpy or
  Genymobile.

## Implementation controls

1. Every subsystem starts from a Mirador design document.
2. The wire protocol is independently specified and versioned before coding.
3. New code cites only platform documentation when a non-obvious API is used.
4. Tests are written from Mirador requirements, not ported from reference tests.
5. Third-party dependencies require a license entry before distribution.
6. Reference source files must never be copied into this project.

## Inspection log

Every later look at the reference is recorded here, with what was read, why,
and what came out of it. The controls above apply to each entry.

### 2026-09-17 - why the mirror stays black on some phones

- **What was read.** From `Genymobile/scrcpy` at `master` (reported version
  4.1), fetched from GitHub into a scratch folder outside this tree and
  deleted afterwards: the screen-capture class, the display-manager and
  surface-control wrappers, the fake context and workarounds, the surface
  encoder, the server entry point and the client's server-connection code.
  Also read, as platform documentation: the AOSP `DisplayManager.java`
  (Android 13 to 16 tags), `DisplayManagerService.java` and
  `VirtualDisplayAdapter.java` (Android 14) and the Shell package manifest
  (Android 13 to 16), all Apache-2.0, from the AOSP mirror.
- **Why.** A phone with Android 14 showed a black window; the task was to
  understand what makes the reference tolerant of phones this product was
  not.
- **What was learned, at the level of behaviour.** (1) The reference reports
  every server-side failure to the person and stops; this product dropped
  the connection without a word and retried for ever. (2) The reference
  waits for its server with a long, connection-based budget; this product
  had a five-second budget that included VM start-up. (3) The reference
  constrains the capture size by what the encoder declares and steps down
  through smaller sizes when `configure` fails; this product configured
  blindly. (4) On Android 14 and later the reference uses the platform's
  own static mirroring helper in `DisplayManager`, a private auto-mirroring
  display; this product asked for a public one.
- **What was written.** All four behaviours were implemented from scratch in
  this product's own structure and names: `HostSignal` and the alive/ready/
  unsupported stdout protocol, the two host budgets, the encoder size
  negotiation in `VideoStreamMain` (own fallback ladder, own fitting rules),
  the third door in `MirrorDisplay` (which calls a documented Android API,
  not anything of the reference's), the 503 answer in the host and the
  failure panel in the viewer.
- **What was NOT taken.** No source, no names, no constants, no fallback
  values, no messages, no wire formats. Nothing from the reference is
  redistributed, so `THIRD_PARTY_LICENSES.txt` and the engine notices are
  unchanged. Had any line been copied or translated, it would have to be
  listed there under scrcpy's Apache-2.0 licence with its notice; none was.

### 2026-09-28 - trying more than one encoder

- **What was read.** No source. The reference's public user documentation
  and public issue reports, read during the 2026-09-26/28 design sessions,
  at the level of behaviour: the documentation says the default encoder
  may fail or hang on some devices and lets the user pick another one.
  Also, as platform documentation: `MediaCodecList` and `MediaCodecInfo`
  (`isSoftwareOnly`, `isAlias`, `getBitrateRange`) in the Android SDK
  reference.
- **Why.** Of the general mechanisms that make the reference tolerant of
  unknown phones, this product lacked one: when its only encoder refused
  every size, the phone could not mirror at all.
- **What was learned, at the level of behaviour.** A phone can carry more
  than one H.264 encoder, and when one fails another may work.
- **What was written.** From scratch: `EncoderOrder` (this product's own
  order - the system's first encoder, then other hardware, then software,
  the one that already worked in front on a rebuild, aliases dropped) and
  the per-encoder ladder in `VideoStreamMain`, which lowers the bit rate of
  a fallback encoder to what it declares. The reference lets a person name
  an encoder by hand; this product chooses on its own and never asks. The
  test hooks `MIRRYL_ENCODERS` / `MIRRYL_ENCODER_REFUSE` follow this
  product's own `MIRRYL_MIRROR_DOORS` pattern.
- **What was NOT taken.** No source, no names, no option syntax, no
  messages. `THIRD_PARTY_LICENSES.txt` and the engine notices are unchanged.

## Scope: this file is about scrcpy only

One other part of the product was developed by reading someone else's source,
and it is **not** covered by the boundary above. The QR-code pairing module
(`app/src-tauri/src/qr_pairing.rs`) implements the Android `adb-tls-pairing`
protocol and was written by reading the Apache-2.0 AOSP `adb` sources, which
the file itself states in its header. That is a different situation with a
different licence and a different answer: it is disclosed and attributed in
Section 2.7 of `THIRD_PARTY_LICENSES.txt`. Do not let the two get conflated —
this document does not authorise or describe that work.

## Legal note

This file records engineering controls and is not legal advice. A commercial
release should receive jurisdiction-specific counsel and a dependency audit.
