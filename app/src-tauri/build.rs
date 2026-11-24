fn main() {
    // MUST run before try_build below: tauri-build checks that every
    // bundle.resources path in tauri.conf.json already exists on disk at
    // this point (it panics with "resource path ... doesn't exist"
    // otherwise).
    stage_bundled_runtime_files();
    stage_phone_app();
    stamp_third_party_notices();

    // The manifest requests asInvoker, not elevation: the app deliberately
    // does NOT prompt for admin at launch. Only firewall_fix/install_watcher
    // elevate, on demand, from a button.
    let attributes = tauri_build::Attributes::new().windows_attributes(
        tauri_build::WindowsAttributes::new()
            .app_manifest(include_str!("windows-app-manifest.xml")),
    );
    tauri_build::try_build(attributes).expect("failed to run tauri-build");
}

/// This crate's folder, read when the build script RUNS, not when it is
/// compiled. `env!("CARGO_MANIFEST_DIR")` is fixed into the build-script binary,
/// and cargo reuses that binary for a copy of the same crate in another folder
/// that shares the target dir: the copy would stage the engine and the phone
/// app into the ORIGINAL folder and fail on its own missing resources.
/// Cargo sets the variable for every run of a build script.
fn manifest_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR for build scripts"),
    )
}

/// The profile folder (`<target>/release`) a build script's OUT_DIR sits in:
/// the parent of its `build` folder. Cargo has two layouts below it,
/// `build/<pkg>-<hash>/out` and, from Rust 1.100, `build/<pkg>/<hash>/out`,
/// so the depth is not fixed.
fn profile_dir_of(out_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    out_dir
        .ancestors()
        .skip(1)
        .find(|p| p.file_name().is_some_and(|name| name == "build"))
        .and_then(|build| build.parent())
        .map(|p| p.to_path_buf())
}

/// Where the pieces around this crate live. Two layouts are recognised:
///
/// - the working tree (`.../codigo/app/src-tauri`): the runtime binaries
///   (WebView2Loader.dll, platform-tools) in `<root>/app`, the engine in
///   `<root>/codigo/motor`, and the phone app in the sibling folder
///   `Mirryl-Android-codigo-fuente` next to `<root>`;
/// - the public repository (`<repo>/app/src-tauri`): the runtime binaries in
///   `<repo>/runtime` (not committed: see docs/BUILD.md), the engine in
///   `<repo>/engine`, and the phone app in `<repo>/android`.
struct Layout {
    runtime_dir: std::path::PathBuf,
    engine_src_dir: std::path::PathBuf,
    android_dir: std::path::PathBuf,
}

fn layout() -> Layout {
    let crate_dir = manifest_dir();
    let above_app = crate_dir
        .parent() // .../app
        .and_then(|p| p.parent()) // .../codigo, or the repository root
        .expect("src-tauri is not nested under an app folder as expected")
        .to_path_buf();
    if above_app.file_name().is_some_and(|name| name == "codigo") {
        let root = above_app.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        Layout {
            runtime_dir: root.join("app"),
            engine_src_dir: above_app.join("motor"),
            android_dir: root
                .parent()
                .map(|p| p.join("Mirryl-Android-codigo-fuente"))
                .unwrap_or_default(),
        }
    } else {
        Layout {
            runtime_dir: above_app.join("runtime"),
            engine_src_dir: above_app.join("engine"),
            android_dir: above_app.join("android"),
        }
    }
}

/// `bundle.resources` in tauri.conf.json can only see files inside this
/// crate (src-tauri) - it has no way to reach the engine's own build output
/// or the vendored adb tools, which both live outside this crate's tree.
/// Without this, an installer build silently ships with no `engine\` and no
/// `platform-tools\` at all, and the app opens to "Couldn't open the
/// mirror... engine\mirryl-engine.exe was not found next to this app".
///
/// Stages fresh copies into src-tauri\ on every build, so bundle.resources
/// always has something real to glob/map - never relies on someone
/// remembering a manual copy before running `cargo tauri build`.
fn stage_bundled_runtime_files() {
    use std::path::PathBuf;

    let crate_dir = manifest_dir();
    let layout = layout();

    // Off Windows the engine's binary is `mirador` and the engine is staged
    // without .exe; WebKitGTK and adb are system packages, so neither
    // WebView2Loader.dll nor platform-tools is staged there. Asked of the
    // TARGET (CARGO_CFG_WINDOWS), not of the machine running this script.
    let for_windows = std::env::var_os("CARGO_CFG_WINDOWS").is_some();
    let (built_bin, engine_bin) = if for_windows {
        ("mirador.exe", "mirryl-engine.exe")
    } else {
        ("mirador", "mirryl-engine")
    };

    let engine_webview2 = layout.runtime_dir.join("engine/WebView2Loader.dll");
    let platform_tools_src = layout.runtime_dir.join("platform-tools");

    let engine_dir = crate_dir.join("engine");
    let platform_tools_dir = crate_dir.join("platform-tools");

    // Where the engine binary may be. Picking the wrong one is SILENT: the
    // build instructions (rightly - a synced folder breaks full rebuilds) send
    // the engine build to a CARGO_TARGET_DIR outside the source tree, and an
    // app that looked only at the fixed in-tree path would build clean around
    // a stale binary. Candidates:
    //   1. MIRRYL_ENGINE_EXE - explicit override. If it points at a file that
    //      EXISTS it wins outright, mtime never consulted (see below).
    //   2. The profile dir of the target THIS build writes to, derived from
    //      OUT_DIR (see profile_dir_of). The engine is built with the same CARGO_TARGET_DIR
    //      and its binary is mirador.exe, so that is where it lands.
    //   3. The fixed in-tree path, for an engine built there.
    // Between 2 and 3 the NEWEST existing one wins and the order above only
    // breaks ties.
    println!("cargo:rerun-if-env-changed=MIRRYL_ENGINE_EXE");
    const OVERRIDE_LABEL: &str = "MIRRYL_ENGINE_EXE";
    let mut candidates: Vec<(&str, PathBuf)> = Vec::new();
    if let Some(p) = std::env::var_os("MIRRYL_ENGINE_EXE") {
        let p = PathBuf::from(p);
        if !p.is_file() {
            // An override that points nowhere would otherwise fall through
            // to the other candidates without a word - the same silence
            // this whole block exists to remove.
            println!(
                "cargo:warning=MIRRYL_ENGINE_EXE is set but {} does not exist - falling back to the other engine candidates",
                p.display()
            );
        }
        candidates.push((OVERRIDE_LABEL, p));
    }
    if let Some(profile_dir) = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .and_then(|out| profile_dir_of(&out))
    {
        candidates.push(("shared target dir", profile_dir.join(built_bin)));
    }
    candidates.push((
        "fixed in-tree path",
        layout.engine_src_dir.join("target/release").join(built_bin),
    ));

    // DELIBERATE, do not "fix" it: watching paths that do NOT exist yet leaves
    // this crate permanently dirty (`cargo check -v` prints `Dirty ...: the
    // file ...\release\mirador.exe is missing`), so build.rs and tauri-build
    // re-run on every single compile. That re-run is exactly what makes the
    // engine's FIRST appearance in the shared target dir get noticed and
    // staged. Emitting only the paths that already exist would make the
    // recipe's own engine step invisible until some unrelated change happened
    // to trigger a rebuild. It costs a few seconds and it cures itself the
    // moment the engine exists there.
    for (_, path) in &candidates {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let existing: Vec<(&str, &PathBuf, std::time::SystemTime)> = candidates
        .iter()
        .filter_map(|(label, path)| {
            let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
            Some((*label, path, mtime))
        })
        .collect();
    // An existing MIRRYL_ENGINE_EXE wins outright - its mtime is never
    // compared: an override that can lose a "newest wins" vote is not an
    // override.
    let chosen = existing
        .iter()
        .find(|(label, _, _)| *label == OVERRIDE_LABEL)
        .or_else(|| {
            // Between the other two: strictly newer replaces, so a tie keeps
            // the more preferred candidate.
            let mut best: Option<&(&str, &PathBuf, std::time::SystemTime)> = None;
            for c in &existing {
                if best.map_or(true, |b| c.2 > b.2) {
                    best = Some(c);
                }
            }
            best
        });

    if let Some((label, engine_exe, mtime)) = chosen {
        // Always say which one got packaged.
        println!(
            "cargo:warning=staging {engine_bin} from {} ({}), modified {}",
            engine_exe.display(),
            label,
            fmt_mtime(*mtime)
        );
        for (other_label, other_path, other_mtime) in &existing {
            if *other_path != *engine_exe {
                if *other_mtime > *mtime {
                    // Only reachable through the override branch: the other
                    // two are chosen by mtime, so nothing newer can be left
                    // behind there. Say it out loud: "my override won and it
                    // is stale" must not pass in silence.
                    println!(
                        "cargo:warning=MIRRYL_ENGINE_EXE overrode a NEWER engine at {} ({}), modified {} - unset the variable to package that one instead",
                        other_path.display(),
                        other_label,
                        fmt_mtime(*other_mtime)
                    );
                } else {
                    println!(
                        "cargo:warning=ignored older engine at {} ({}), modified {}",
                        other_path.display(),
                        other_label,
                        fmt_mtime(*other_mtime)
                    );
                }
            }
        }
        let _ = std::fs::create_dir_all(&engine_dir);
        if let Err(e) = std::fs::copy(engine_exe, engine_dir.join(engine_bin)) {
            println!("cargo:warning=could not stage {engine_bin}: {e}");
        }
        if !for_windows {
            // WebKitGTK is a system library: nothing to ship beside the engine.
        } else if engine_webview2.exists() {
            if let Err(e) = std::fs::copy(&engine_webview2, engine_dir.join("WebView2Loader.dll")) {
                println!("cargo:warning=could not stage engine's WebView2Loader.dll: {e}");
            }
        } else {
            println!(
                "cargo:warning=engine WebView2Loader.dll not found at {} - engine window will fail to open",
                engine_webview2.display()
            );
        }
    } else {
        // Deliberately does NOT fail plain `cargo build` - most app-only
        // iteration never touches the engine. `bundle.resources` pointing at
        // a nonexistent engine\ is what makes `cargo tauri build` itself
        // fail loudly when it matters (packaging for real).
        //
        // Lists EVERY path that was tried, not just the fixed one: the build
        // instructions build the engine into CARGO_TARGET_DIR, not in-tree.
        let tried = candidates
            .iter()
            .map(|(label, path)| format!("{} ({})", path.display(), label))
            .collect::<Vec<_>>()
            .join("; ");
        println!(
            "cargo:warning=engine build not found - looked at: {tried} - build it first (cargo build --release in {}) or the installer will ship without the mirroring engine",
            layout.engine_src_dir.display()
        );
    }

    if !for_windows {
        // The system adb (a dependency of the Linux package) is what runs there.
    } else if platform_tools_src.is_dir() {
        let _ = std::fs::create_dir_all(&platform_tools_dir);
        match std::fs::read_dir(&platform_tools_src) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        if let Some(name) = path.file_name() {
                            if let Err(e) = std::fs::copy(&path, platform_tools_dir.join(name)) {
                                println!("cargo:warning=could not stage {}: {e}", path.display());
                            }
                        }
                    }
                }
            }
            Err(e) => println!("cargo:warning=could not read {}: {e}", platform_tools_src.display()),
        }
    } else {
        println!(
            "cargo:warning=platform-tools not found at {} - the installer will ship without adb",
            platform_tools_src.display()
        );
    }
}

/// Stages the signed phone app into `src-tauri/phone-app/` (it is not
/// published on its own anywhere; this PC app carries it and installs it with
/// a button). Two files, always together: `Mirryl.apk` and
/// `Mirryl.apk.version`, the versionCode that the Android build script writes
/// next to the APK it signs.
///
/// Where the APK comes from: `MIRRYL_PHONE_APK` if set, else the Android
/// tree's own output, `build/Mirryl.apk` under the phone app folder that
/// `layout()` finds (`Mirryl-Android-codigo-fuente` in the working tree,
/// `android` in the public repository).
///
/// Without an APK, or without its version file, nothing is staged and any
/// copy staged before is REMOVED: an old APK left behind would be packaged
/// silently with whatever version file sat next to it. The `bundle.resources`
/// entries then point nowhere, and tauri-build stops every build of the app,
/// `cargo test` included ("resource path `phone-app/Mirryl.apk` doesn't
/// exist", the same check that guards engine\ and platform-tools\): build the
/// APK first.
fn stage_phone_app() {
    use std::path::PathBuf;

    const APK: &str = "Mirryl.apk";
    const VERSION: &str = "Mirryl.apk.version";
    let crate_dir = manifest_dir();
    let staged_dir = crate_dir.join("phone-app");

    println!("cargo:rerun-if-env-changed=MIRRYL_PHONE_APK");
    let source_apk = match std::env::var_os("MIRRYL_PHONE_APK") {
        Some(p) => PathBuf::from(p),
        None => layout().android_dir.join("build").join(APK),
    };
    let source_version = source_apk.with_file_name(VERSION);
    println!("cargo:rerun-if-changed={}", source_apk.display());
    println!("cargo:rerun-if-changed={}", source_version.display());

    let version = std::fs::read_to_string(&source_version)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    let unstage = |why: String| {
        let _ = std::fs::remove_file(staged_dir.join(APK));
        let _ = std::fs::remove_file(staged_dir.join(VERSION));
        println!("cargo:warning={why} - the installer cannot be packaged without the phone app");
    };
    if !source_apk.is_file() {
        unstage(format!("phone app not found at {}", source_apk.display()));
        return;
    }
    let Some(version) = version else {
        unstage(format!(
            "{} has no readable {VERSION} next to it (build the APK with build.ps1 / build.sh, which writes it)",
            source_apk.display()
        ));
        return;
    };
    let _ = std::fs::create_dir_all(&staged_dir);
    if let Err(e) = std::fs::copy(&source_apk, staged_dir.join(APK)) {
        unstage(format!("could not stage {}: {e}", source_apk.display()));
        return;
    }
    if let Err(e) = std::fs::write(staged_dir.join(VERSION), format!("{version}\n")) {
        unstage(format!("could not stage {VERSION}: {e}"));
        return;
    }
    let mtime = std::fs::metadata(&source_apk)
        .and_then(|m| m.modified())
        .map(fmt_mtime)
        .unwrap_or_else(|_| "unknown mtime".to_string());
    println!(
        "cargo:warning=staging phone app versionCode {version} from {}, modified {mtime}",
        source_apk.display()
    );
}

/// Renders a file mtime as `YYYY-MM-DD HH:MM:SS UTC` for the staging
/// warnings above. Done by hand because build-dependencies has no date
/// crate and adding one is off the table (nothing new in Cargo.lock).
/// Civil-from-days is Howard Hinnant's algorithm, proleptic Gregorian.
fn fmt_mtime(t: std::time::SystemTime) -> String {
    let secs = match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => return "unknown mtime".to_string(),
    };
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// Keeps line 2 of THIRD_PARTY_LICENSES.txt ("Applies to: Mirryl <version>")
/// equal to the version being built: a stamp nobody has to remember cannot be
/// forgotten. The file is a bundle resource, so it is rewritten in place -
/// only when the line actually differs, so a build with the right stamp
/// touches nothing.
fn stamp_third_party_notices() {
    // The Linux package ships its own notices (THIRD_PARTY_LICENSES-linux.txt,
    // packaged under the usual name by tauri.linux.conf.json); same stamp.
    stamp_notices_file("THIRD_PARTY_LICENSES.txt");
    stamp_notices_file("THIRD_PARTY_LICENSES-linux.txt");
}

fn stamp_notices_file(name: &str) {
    let path = manifest_dir().join(name);
    println!("cargo:rerun-if-changed={}", path.display());
    let version = env!("CARGO_PKG_VERSION");
    let Ok(text) = std::fs::read_to_string(&path) else {
        println!("cargo:warning={name} not found; version stamp not checked");
        return;
    };
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.split(newline).map(str::to_string).collect();
    let wanted = format!("Applies to: Mirryl {version}");
    match lines.get(1) {
        Some(line) if line.starts_with("Applies to: Mirryl ") && *line != wanted => {
            println!("cargo:warning={name} said \"{line}\"; stamped \"{wanted}\"");
            lines[1] = wanted;
            if let Err(error) = std::fs::write(&path, lines.join(newline)) {
                println!("cargo:warning=could not stamp {name}: {error}");
            }
        }
        Some(line) if line.starts_with("Applies to: Mirryl ") => {}
        _ => println!("cargo:warning={name} line 2 is not an \"Applies to: Mirryl\" stamp; left alone"),
    }
}
