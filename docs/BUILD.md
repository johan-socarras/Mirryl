# Building Mirryl

Each version is built on its own system: the Windows installer on Windows, the
Linux `.deb` on Linux. Both come from the same sources, in three builds and in
this order: the phone app, the mirroring engine, and the PC app, which
packages the other two.

- [Windows](#windows)
- [Linux](#linux)
- [Legal texts](#legal-texts)

## Windows

### Prerequisites

- **Rust** (stable) with the GNU host toolchain, `x86_64-pc-windows-gnu`.
  The whole build assumes GNU, not MSVC:

  ```powershell
  winget install --id Rustlang.Rustup -e --override "--default-host x86_64-pc-windows-gnu --default-toolchain stable -y"
  ```

- **MinGW-w64** as the linker, for example WinLibs:

  ```powershell
  winget install --id BrechtSanders.WinLibs.POSIX.MSVCRT -e
  ```

- **Tauri CLI 2.11**: `cargo install tauri-cli --version "=2.11.4" --locked`. The installer template is written for its NSIS bundler; later versions stop with `macro named "RestartManager_StartSession" not found`.
- **JDK** (17 or newer) and the **Android SDK** with one `build-tools` and one
  `platforms` package installed (Android Studio's SDK Manager is the easy way).
  No Gradle is needed.
- **Android SDK Platform-Tools** (adb), from
  [developer.android.com](https://developer.android.com/tools/releases/platform-tools).

### Environment

Every `cargo` command below runs with these variables set (PowerShell; adjust
the WinLibs path to where it was installed):

```powershell
$wl = "<WinLibs>\mingw64\bin"
$env:PATH = "$wl;$env:USERPROFILE\.cargo\bin;$env:PATH"
$env:RUSTFLAGS = "-Clink-self-contained=no -Clinker=$wl\x86_64-w64-mingw32-gcc.exe -Cdlltool=$wl\dlltool.exe --remap-path-prefix=$env:USERPROFILE\.cargo=/cargo"
$env:CARGO_TARGET_DIR = "$env:USERPROFILE\mirryl-build-target"
```

- `--remap-path-prefix` keeps your user folder's path out of the binaries.
- `CARGO_TARGET_DIR` must be outside any folder synced to the cloud (a full
  rebuild inside one tends to fail), and the engine and the app must share it:
  the app's build picks the engine up from there.

### 1. The phone app

```powershell
cd android
.\create-keystore.ps1   # once: creates the signing key outside the project
.\build.ps1
```

`create-keystore.ps1` asks for a password and creates the keystore in your
user folder (or where `MIRRYL_KEYSTORE` points). Keep the file and its
password safe: phones only accept updates signed with the same key.
`build.ps1` writes `android/build/Mirryl.apk` and `Mirryl.apk.version`.

### 2. The engine

```powershell
cd engine
cargo test --release
cargo build --release
```

This produces `mirador.exe` and `WebView2Loader.dll` in
`$env:CARGO_TARGET_DIR\release`.

The engine embeds the phone-side agent, `engine/assets/mirryl-stream-agent.jar`,
which is committed. Only if you change the Java agent in `engine/agent/`,
rebuild it first with `engine\scripts\build-agent.ps1`.

### 3. Runtime files

The installer carries two things that are not built from this repository.
Put them in a `runtime/` folder at the repository root (it is ignored by git):

```
runtime/
├── engine/
│   └── WebView2Loader.dll   <- copy from $env:CARGO_TARGET_DIR\release
└── platform-tools/
    ├── adb.exe              <- from Android SDK Platform-Tools
    ├── AdbWinApi.dll
    ├── AdbWinUsbApi.dll
    └── NOTICE.txt
```

### 4. The app and the installer

```powershell
cd app\src-tauri
cargo test --release
cargo tauri build
```

One of the app's tests runs `adb keygen`, so adb must be on `PATH` for
`cargo test` (for example the SDK's `platform-tools` folder).

The build prints which engine and which APK it packaged:

```
warning: staging mirryl-engine.exe from ...\release\mirador.exe ...
warning: staging phone app versionCode 27 from ...\android\build\Mirryl.apk ...
```

The installer is written to
`$env:CARGO_TARGET_DIR\release\bundle\nsis\Mirryl_<version>_x64-setup.exe`.

Two variables override where the pieces come from: `MIRRYL_ENGINE_EXE` (a
specific `mirador.exe`) and `MIRRYL_PHONE_APK` (a specific `Mirryl.apk`, with
its `Mirryl.apk.version` next to it). Without the APK the build stops on
purpose, so an old phone app can never slip into an installer.

The build also prints `.rsrc merge failure: multiple non-default manifests`;
that warning is expected and harmless.

## Linux

The commands are for Ubuntu and other Debian-based systems.

### Prerequisites

- **Rust** (stable), from [rustup.rs](https://rustup.rs).
- **The libraries Tauri and the engine build against**:

  ```bash
  sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libssl-dev
  ```

- **Tauri CLI 2**: `cargo install tauri-cli --version "^2"`.
- **JDK** (17 or newer, for example `sudo apt install default-jdk-headless`)
  and the **Android SDK** with one `build-tools` and one `platforms` package
  installed, in `~/Android/Sdk` or where `ANDROID_HOME` points.
- To run what you build: `sudo apt install adb zenity gstreamer1.0-libav gstreamer1.0-plugins-good`
  (the `.deb` depends on them, so installing it brings them in too).

### Environment

```bash
export CARGO_TARGET_DIR="$HOME/mirryl-build-target"
export RUSTFLAGS="--remap-path-prefix=$HOME/.cargo=/cargo"
```

The engine and the app must share `CARGO_TARGET_DIR`: the app's build picks
the engine up from there.

### 1. The phone app

The signing key is created once with `keytool`, outside the project, and
`build.sh` signs with it (it asks for the password, or reads
`MIRRYL_KEYSTORE_PASS`):

```bash
cd android
export MIRRYL_KEYSTORE="$HOME/Mirryl-keys/mirryl-phone-app.keystore"
mkdir -p "$(dirname "$MIRRYL_KEYSTORE")"
keytool -genkeypair -keystore "$MIRRYL_KEYSTORE" -storetype PKCS12 -alias mirryl \
    -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=Mirryl, O=Mirryl"   # once
./build.sh
```

Keep the keystore and its password safe: phones only accept updates signed
with the same key. `build.sh` writes `android/build/Mirryl.apk` and
`Mirryl.apk.version`.

### 2. The engine

```bash
cd engine
cargo test --release
cargo build --release
```

This produces `mirador` in `$CARGO_TARGET_DIR/release`.

### 3. The app and the package

```bash
cd app/src-tauri
cargo test --release
cargo tauri build --bundles deb
```

One of the app's tests runs `adb keygen`, so adb must be on `PATH` for
`cargo test` (`sudo apt install adb`).

Linux needs no `runtime/` folder: WebKitGTK and adb are system packages.
The package is written to
`$CARGO_TARGET_DIR/release/bundle/deb/Mirryl_<version>_amd64.deb`. The same
two variables as on Windows, `MIRRYL_ENGINE_EXE` (here a specific `mirador`)
and `MIRRYL_PHONE_APK`, override where the pieces come from.

## Legal texts

`app/src-tauri/EULA.txt` is the source, the same for both systems; after
changing it, regenerate the installer's ASCII copy with
`python gen-eula-installer.py` (in `app/src-tauri`). The privacy notice and the
third-party notices have one file per system: `PRIVACY.txt`,
`THIRD_PARTY_LICENSES.txt` and `COMPONENT-INVENTORY.txt` for Windows, and the
same names ending in `-linux` for Linux, which the `.deb` installs under the
plain names.
