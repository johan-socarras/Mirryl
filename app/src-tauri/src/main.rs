// Always build as a windowed (GUI) app, in debug and release alike: a console
// window would open beside it, and closing that window would take the whole
// app down with it.
#![windows_subsystem = "windows"]

mod apps;
mod matriculation;
mod qr_pairing;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Manager;
use wait_timeout::ChildExt;

const PACKAGE_NAME: &str = "com.mirryl.app";
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (the watcher's scheduled task).
const WATCHER_TASK_NAME: &str = "AutoWifiADB-Watcher";
const PROCESS_TIMEOUT: Duration = Duration::from_secs(40);

#[cfg(windows)]
const ADB_BUNDLED_NAME: &str = "adb.exe";
#[cfg(not(windows))]
const ADB_BUNDLED_NAME: &str = "adb";

/// Mirroring engine bundled under `engine/`: Mirryl Connect, Mirryl's own
/// screen engine (its own Android agent, H.264/MYV1 video, PCM audio and MYC1
/// control over adb).
#[cfg(windows)]
const ENGINE_BUNDLED_NAME: &str = "mirryl-engine.exe";
/// Linux runs the same engine, built for it: a GTK window with WebKitGTK.
#[cfg(not(windows))]
const ENGINE_BUNDLED_NAME: &str = "mirryl-engine";

/// Where what the installer shipped lives: the exe's own folder on Windows.
///
/// The Linux package puts the program in `/usr/bin` and everything Windows
/// ships beside it (engine/, phone-app/, the license texts) in
/// `/usr/lib/Mirryl`, so that is where this looks when the exe's folder has
/// no engine. A folder that has one (a portable copy, a build) is used as is.
fn app_dir() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let dir = exe.parent()?.to_path_buf();
    #[cfg(target_os = "linux")]
    if !dir.join("engine").exists() {
        for candidate in [dir.join("../lib/Mirryl"), PathBuf::from("/usr/lib/Mirryl")] {
            if candidate.join("engine").exists() {
                return Some(candidate.canonicalize().unwrap_or(candidate));
            }
        }
    }
    Some(dir)
}

/// The folder Tauri's `app_data_dir()` gives this app, for the code that has
/// no `AppHandle` to ask: `%APPDATA%\com.awadb.assistant` on Windows,
/// `~/.local/share/com.awadb.assistant` on Linux. The settings the engine is
/// handed (keybindings, window position, the game hint) and the files staged
/// for the phone live here.
fn roaming_app_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = dirs::data_dir();
    base.map(|dir| dir.join("com.awadb.assistant"))
}

/// Where this app WRITES what it produces while running - logs, the scratch
/// scripts, the per-mirror transport files. Never `app_dir()`, which is only
/// for what the installer shipped.
///
/// For the installer the two are the same folder: it installs per user into
/// `%LOCALAPPDATA%\Mirryl`. They differ in a Microsoft Store (MSIX) package,
/// whose program folder (`C:\Program Files\WindowsApps\…`) is read-only to
/// everyone, and in the Linux package, which lives under `/usr`.
///
/// Resolved through the Known Folders API (`dirs` → SHGetKnownFolderPath), not
/// `%LOCALAPPDATA%`: an environment variable can be repointed by any process of
/// this user from `HKCU\Environment`, and this folder holds scripts that are
/// about to be executed - see `script_scratch_dir`. Per-user, so another local
/// account has no access to it.
fn local_data_dir() -> Option<PathBuf> {
    let dir = dirs::data_local_dir()?.join("Mirryl");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// True when this copy runs from a Microsoft Store (MSIX) package.
///
/// The package changes what Mirryl must NOT do: the Store deletes the
/// versioned install folder on every update and runs nothing on uninstall, so
/// firewall rules keyed by that folder and a scheduled task pointing into it
/// break on the first update and are left behind forever. In the package both
/// are declared
/// in its manifest instead - `windows.firewallRules` and a `windows.startupTask`
/// that runs `Mirryl.exe --watcher` at sign-in - and Windows creates, updates
/// and removes them itself, without administrator. So in the package nothing
/// here registers the task or creates rules, and the health check has nothing
/// to report about them. The regular install is untouched.
#[cfg(windows)]
fn is_store_package() -> bool {
    static PACKAGED: OnceLock<bool> = OnceLock::new();
    *PACKAGED.get_or_init(|| {
        use windows_sys::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
        use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
        // Asked with an empty buffer on purpose: an unpackaged process gets
        // APPMODEL_ERROR_NO_PACKAGE, a packaged one ERROR_INSUFFICIENT_BUFFER
        // with the length it would need. The name itself is not wanted.
        let mut len: u32 = 0;
        let rc = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
        rc != APPMODEL_ERROR_NO_PACKAGE
    })
}

/// What Repair and the firewall fix answer in the Store package. "Done." first
/// because that is what the UI tests for: nothing failed, there was simply
/// nothing of ours to change.
#[cfg(windows)]
const STORE_FIREWALL_NOTE: &str = "Done. In the Microsoft Store version Windows manages Mirryl's firewall rules and its auto-reconnect watcher; there is nothing to repair here.";

/// The app execution alias the Store manifest declares for its adb.exe
/// (windows.appExecutionAlias). Must match the manifest - see
/// start_store_watcher for why the watcher needs it.
#[cfg(windows)]
const STORE_ADB_ALIAS: &str = "mirryl-adb.exe";

/// What finishing setup answers in the Store package (install_watcher).
#[cfg(windows)]
const STORE_WATCHER_NOTE: &str = "Done. Auto-reconnect is running, and starts every time you sign in to Windows. You can turn it off in Windows Settings > Apps > Startup.";

/// Starts the auto-reconnect watcher as a hidden PowerShell child - the Store
/// package's replacement for the `AutoWifiADB-Watcher` scheduled task.
///
/// Called by `Mirryl.exe --watcher`, which is what the manifest's startup task
/// runs at sign-in, and by `install_watcher` so pairing a phone does not have
/// to wait for the next sign-in. Two at once cannot happen: watcher.ps1 takes a
/// named mutex and the second one exits.
///
/// `-DataDir` because the watcher writes its log beside itself by default, and
/// inside the package that folder is read-only (see `local_data_dir`).
#[cfg(windows)]
fn start_store_watcher() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let watcher = watcher_script_path()
        .ok_or_else(|| "watcher.ps1 was not found next to Mirryl.exe".to_string())?;
    let data_dir =
        local_data_dir().ok_or_else(|| "the Mirryl data folder could not be created".to_string())?;
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&watcher)
        .arg("-DataDir")
        .arg(&data_dir);
    // The package's adb through its app execution alias, not by path: Windows
    // starts this PowerShell OUTSIDE the package (it is a Windows program, not
    // ours), and from there executing a file inside WindowsApps is denied:
    // every adb call of the watcher fails with "Access is denied". The alias,
    // declared in the manifest, runs the same adb.exe
    // inside the package whoever calls it. Checked on disk rather than by
    // is_store_package(): the watcher of an updated version is started by the
    // old one, without package identity, and still needs it.
    if let Some(alias) = dirs::data_local_dir()
        .map(|d| d.join("Microsoft").join("WindowsApps").join(STORE_ADB_ALIAS))
        .filter(|p| p.exists())
    {
        cmd.arg("-AdbPath").arg(alias);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("could not start the watcher: {e}"))
}

/// Prefers the adb bundled alongside this app so it works out of the box
/// without the user installing the Android SDK or fixing PATH themselves.
fn tools_dir() -> Option<PathBuf> {
    let base = app_dir()?;
    let dir = base.join("platform-tools");
    dir.exists().then_some(dir)
}

fn adb_path() -> String {
    if let Some(dir) = tools_dir() {
        let candidate = dir.join(ADB_BUNDLED_NAME);
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }
    // Linux ships no adb: the system's (a dependency of the package) is used,
    // by its full path so the engine is handed the very same binary.
    #[cfg(not(windows))]
    if let Some(found) = find_on_path("adb") {
        return found.to_string_lossy().to_string();
    }
    "adb".to_string()
}

/// The first executable called `name` on PATH, as an absolute path.
#[cfg(not(windows))]
fn find_on_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The path to adb's persistent identity public key (`~/.android/adbkey.pub`),
/// which the QR pairing flow sends to the phone as our "peer info".
fn adb_pubkey_path() -> Option<PathBuf> {
    let home = env::var("USERPROFILE").or_else(|_| env::var("HOME")).ok()?;
    Some(PathBuf::from(home).join(".android").join("adbkey.pub"))
}

/// Make sure adb's identity key exists before the QR pairing flow needs it.
///
/// adb normally generates `~/.android/adbkey[.pub]` the first time its server
/// starts, but a computer that has NEVER run adb has no key yet - and our QR
/// flow reads the public key directly to send as peer info (see
/// qr_pairing::read_own_adb_pubkey), which fails on a fresh machine ("could
/// not read adbkey.pub"). Here we generate it with `adb keygen` (also
/// creating the .android dir first - keygen errors out on a missing parent
/// dir) so pairing works out of the box. Idempotent: a no-op once the key
/// exists.
///
/// Returns a user-actionable error when the key can't be produced - in
/// practice that means adb itself is missing or broken (with no adb, keygen
/// fails silently and the late "could not read adbkey.pub" explains nothing).
/// The QR flow surfaces this immediately instead of failing deep into the
/// handshake.
pub(crate) fn ensure_adb_key() -> Result<(), String> {
    let Some(pubkey) = adb_pubkey_path() else {
        return Err("could not determine the user's home directory".to_string());
    };
    if pubkey.exists() {
        return Ok(());
    }
    if let Some(android_dir) = pubkey.parent() {
        std::fs::create_dir_all(android_dir)
            .map_err(|e| format!("could not create {}: {}", android_dir.display(), e))?;
    }
    // `adb keygen <file>` writes the private key to <file> and the public key
    // to <file>.pub - exactly where adb and our reader expect them.
    let keyfile = pubkey.with_extension(""); // .../adbkey.pub -> .../adbkey
    let output = run_process(&adb_path(), &["keygen", &keyfile.to_string_lossy()]);
    if pubkey.exists() {
        Ok(())
    } else {
        // This message is NOT platform-gated - it reaches Windows users too, so
        // it must not send them to a package manager. adb ships inside
        // platform-tools\ next to the exe, so on Windows a failure here means
        // that folder is missing or blocked, not that adb needs installing.
        Err(format!(
            "adb could not create its pairing key ({}). Check that the platform-tools folder is next to Mirryl.exe and that no antivirus is blocking adb.exe, then try again.",
            output.trim().lines().last().unwrap_or("no output")
        ))
    }
}

/// Resolves the bundled mirroring engine (`engine\mirryl-engine.exe`, or
/// `engine/mirryl-engine` on Linux). Returns None only when the packaging is
/// broken.
fn engine_program() -> Option<PathBuf> {
    let candidate = app_dir()?.join("engine").join(ENGINE_BUNDLED_NAME);
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Resolves watcher.ps1 next to the running exe (installed/bundled case),
/// falling back to the source tree location during `cargo run` in dev.
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (watcher, wireless reconnect).
fn watcher_script_path() -> Option<PathBuf> {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("watcher.ps1");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    #[cfg(debug_assertions)]
    {
        let dev_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("watcher.ps1");
        if dev_candidate.exists() {
            return Some(dev_candidate);
        }
    }
    None
}

/// Resolves THIRD_PARTY_LICENSES.txt in the app's folder (`app_dir`: next to
/// the exe on Windows, `/usr/lib/Mirryl` in the Linux package).
fn third_party_licenses_path() -> Option<PathBuf> {
    if let Some(dir) = app_dir() {
        let candidate = dir.join("THIRD_PARTY_LICENSES.txt");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    #[cfg(debug_assertions)]
    {
        let dev_candidate = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("THIRD_PARTY_LICENSES.txt");
        if dev_candidate.exists() {
            return Some(dev_candidate);
        }
    }
    None
}

#[tauri::command]
fn get_third_party_licenses() -> String {
    match third_party_licenses_path() {
        Some(path) => std::fs::read_to_string(path)
            .unwrap_or_else(|e| format!("Error reading THIRD_PARTY_LICENSES.txt: {}", e)),
        None => "THIRD_PARTY_LICENSES.txt was not found next to this app.".to_string(),
    }
}

/// Runs a process with a hard timeout. adb can hang indefinitely on a
/// network hiccup (verified: even after printing "protocol fault", the
/// process can keep running) - without a timeout, `.output()` would block
/// forever and the UI would sit on "Working..." with no feedback at all.
///
/// Also suppresses the console window on Windows: since this app itself has
/// no console (`windows_subsystem = "windows"`), spawning a console-mode
/// child like adb.exe or powershell.exe would otherwise make Windows open a
/// brand new terminal host to attach to it - a window flashing open and
/// closed on every call, which the 3-second status poll makes constant.
fn run_process(program: &str, args: &[&str]) -> String {
    run_process_timeout(program, args, PROCESS_TIMEOUT)
}

/// Same, with an explicit deadline. Only the elevated path needs its own: that
/// one waits on a UAC dialog a human has to click, and 40 s of thinking time is
/// not a hung process.
fn run_process_timeout(program: &str, args: &[&str], timeout: Duration) -> String {
    let mut command = Command::new(program);
    command.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return format!("Error running {}: {}", program, e),
    };

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(s) = stdout_pipe.as_mut() {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(s) = stderr_pipe.as_mut() {
            let _ = s.read_to_string(&mut buf);
        }
        buf
    });

    let timed_out = match child.wait_timeout(timeout) {
        Ok(Some(_status)) => false,
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            true
        }
        Err(_) => false,
    };

    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    let mut combined = stdout;
    combined.push_str(&stderr);
    let combined = combined.trim().to_string();

    if timed_out {
        format!(
            "Timed out after {}s waiting for {} {}. This usually means the phone is unreachable on the network (wrong IP:Port, phone asleep, or a firewall blocking the connection).\nPartial output:\n{}",
            timeout.as_secs(),
            program,
            args.join(" "),
            combined
        )
    } else {
        combined
    }
}

#[cfg(windows)]
fn run_powershell_with(script: &str, sta: bool, timeout: Duration) -> String {
    use rand::RngCore;
    use std::io::Write;

    // Same reasoning as `run_powershell_elevated`, one privilege level down: the
    // script written here is executed, so neither its directory nor its name may
    // be something another process can predict or choose.
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let tmp = script_scratch_dir().join(format!("ps_{}.ps1", hex::encode(raw)));

    // Prepended to every script rather than written into each one: the
    // console code page is the one thing they all share. Windows PowerShell
    // 5.1 writes stdout in [Console]::OutputEncoding - the OEM page (cp850 on
    // a Spanish Windows) unless the user opted into UTF-8 - and this side
    // reads it as UTF-8, so a file picked with an "ñ" in its name would come
    // back as an empty string (read_to_string failing), i.e. as a cancel.
    let script = format!("[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n{script}");
    // A failure here returns "FAILED", the word every caller tests for, never
    // "": that is also what a script that ran and printed nothing returns, so
    // the health check would read "unknown" and offer no Repair, and
    // --repair-setup would log "ok" for work that never started.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(script.as_bytes()) {
                drop(f);
                let _ = std::fs::remove_file(&tmp);
                return format!("FAILED: could not write the PowerShell script {} ({e})", tmp.display());
            }
        }
        Err(e) => {
            return format!("FAILED: could not create the PowerShell script {} ({e})", tmp.display());
        }
    }
    let path = tmp.to_string_lossy().to_string();
    let mut args = vec!["-NoProfile", "-ExecutionPolicy", "Bypass"];
    if sta {
        args.push("-STA");
    }
    args.push("-File");
    args.push(&path);
    let result = run_process_timeout("powershell.exe", &args, timeout);
    let _ = std::fs::remove_file(&tmp);
    result
}

#[cfg(windows)]
fn run_powershell(script: &str) -> String {
    run_powershell_with(script, false, PROCESS_TIMEOUT)
}

/// How long a native file picker may stay open. It is a person choosing, not
/// a process working, so this is a backstop against a picker that never came
/// up at all, not a budget: PROCESS_TIMEOUT (40 s) would kill the picker under
/// the user while they browse - see pick_file.
#[cfg(windows)]
const PICKER_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// WinForms (used for the native file picker) requires the single-threaded
/// apartment model - without -STA, OpenFileDialog either throws or silently
/// fails to show.
#[cfg(windows)]
fn run_powershell_sta(script: &str) -> String {
    run_powershell_with(script, true, PICKER_TIMEOUT)
}

/// Prefix every caller can test for: the user saw the UAC dialog and said no.
/// That is a decision, not a failure, and nothing was changed - so setup keeps
/// going instead of treating it as a broken install.
#[cfg(windows)]
const UAC_DENIED_PREFIX: &str = "Skipped.";

/// Prefix for the other outcome that is neither success nor failure: the
/// elevated step outlived ELEVATED_TIMEOUT, so this process stopped waiting -
/// but the elevated child is at a higher integrity level and cannot be killed
/// from here, so it is still running and will still finish.
///
/// It must not read as UAC_DENIED_PREFIX's "nothing was changed": the user
/// granted the prompt and the script is busy changing things. That matters
/// most for firewall_repair_script, which DELETES the rules before recreating
/// them, so the firewall can sit half-rebuilt while the step finishes.
#[cfg(windows)]
const ELEVATED_STILL_RUNNING_PREFIX: &str = "Still working.";

/// The outcomes a caller must hand back to the UI verbatim: neither is a
/// failure, and neither may be dressed up as "Done." - one changed nothing,
/// the other has not finished changing things yet.
#[cfg(windows)]
fn is_elevated_early_return(out: &str) -> bool {
    out.starts_with(UAC_DENIED_PREFIX) || out.starts_with(ELEVATED_STILL_RUNNING_PREFIX)
}

/// A UAC prompt plus real work behind it. Generous on purpose: the clock starts
/// when the dialog appears, and a person walking back to their desk is not a
/// hung process.
#[cfg(windows)]
const ELEVATED_TIMEOUT: Duration = Duration::from_secs(180);

/// Quotes a string as a PowerShell **single-quoted** literal.
///
/// Single quotes are the only PowerShell string form that expands nothing: no
/// `$(...)`, no `$var`, no backtick escapes. Everything that reaches an elevated
/// script through interpolation goes through here.
///
/// A path interpolated raw into a DOUBLE-quoted string would expand: a
/// directory whose *name* contains `$(...)` would run as Administrator,
/// because the script holding it is the one launched with `-Verb RunAs`, and
/// paths under `%TMP%`/`%TEMP%` can be repointed by any non-elevated process
/// of this user through `HKCU\Environment`.
/// Doubling an embedded quote is the only escape a single-quoted literal needs.
#[cfg(windows)]
fn ps_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Where every PowerShell script this app writes and then runs lives, elevated
/// or not.
///
/// In `local_data_dir()` - `%LOCALAPPDATA%\Mirryl\elevated`, resolved through
/// the Known Folders API - and deliberately NOT `env::temp_dir()`: a process
/// that is not us must not get to choose the path of a file we are about to
/// execute. `%TMP%` can be repointed from `HKCU\Environment` without elevating;
/// the known folder cannot be steered by an environment variable.
///
/// It matters for the non-elevated runner too: `%TEMP%` can carry an ACL that
/// gives another local account Modify (sandbox groups do), and that account
/// could rewrite a script that then runs as this user. `%LOCALAPPDATA%` is
/// this user's alone.
///
/// The fallback to the temp directory is only for the case where the known
/// folder cannot be resolved or created at all. It is safe by itself because the
/// file name is random and `ps_single_quote` already removes the injection.
#[cfg(windows)]
fn script_scratch_dir() -> PathBuf {
    let base = local_data_dir().unwrap_or_else(env::temp_dir);
    let dir = base.join("elevated");
    let _ = std::fs::create_dir_all(&dir);
    // Unlike %TEMP%, nothing sweeps this folder on its own, and the timeout
    // path leaves these files behind on
    // purpose (see run_powershell_elevated). Best-effort sweep of anything older
    // than a day, so a long-lived install does not accumulate them.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let day = Duration::from_secs(86_400);
        for entry in entries.flatten() {
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|m| m.elapsed().map(|age| age > day).unwrap_or(false))
                .unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    dir
}

/// Runs a PowerShell script **as administrator**, prompting for UAC.
///
/// The app itself is asInvoker (see windows-app-manifest.xml), so the two
/// operations that genuinely need rights - firewall rules and registering the
/// watcher task - come through here, and only when the user asks for them.
///
/// Why the output travels through a file: `Start-Process -Verb RunAs` starts a
/// process at a *higher* integrity level, and we do not get its stdout - the
/// pipe would be a handle a lower-integrity process handed to a higher one.
/// So the elevated payload writes its own output where we can read it back.
#[cfg(windows)]
fn run_powershell_elevated(script: &str) -> String {
    use rand::RngCore;
    use std::io::Write;

    let dir = script_scratch_dir();
    // Random, not derived from the clock: a name built from the time is
    // predictable enough to pre-create or replace in the window between
    // writing the file and the elevated child
    // reading it - and that window is the whole UAC dialog. 128 bits from the
    // OS generator closes it; `create_new` below turns a collision, or a
    // planted file, into a clean failure instead of a file we would then run.
    let mut raw = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let tag = hex::encode(raw);
    let payload = dir.join(format!("elev_{tag}.ps1"));
    let outfile = dir.join(format!("elev_{tag}.out"));

    // `& { ... } *>&1` captures every stream (a PowerShell error written to the
    // error stream would otherwise vanish and look like silence).
    //
    // `-LiteralPath` with a single-quoted argument, not `-FilePath "..."`: see
    // `ps_single_quote`. A double-quoted path here would be a privilege
    // escalation.
    let wrapped = format!(
        "$ErrorActionPreference = \"Continue\"\r\n& {{\r\n{script}\r\n}} *>&1 | Out-File -LiteralPath {out} -Encoding utf8\r\n",
        script = script,
        out = ps_single_quote(&outfile.to_string_lossy())
    );
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&payload)
    {
        Ok(mut f) => {
            let _ = f.write_all(wrapped.as_bytes());
        }
        Err(_) => {
            return "Error: could not write the temporary script for the elevated step.".to_string();
        }
    }

    // Quoting note: ArgumentList is ONE string with the path quoted inside it.
    // Passing an array would join the elements with spaces and no quoting, so a
    // user whose name has a space ("C:\Users\Ana Maria\...") would break.
    let launcher = format!(
        r#"
try {{
    # -ErrorAction Stop so that cancelling the UAC dialog lands in catch rather
    # than being a non-terminating error that falls through silently. Without it
    # the only signal is an output file that never appears, which is also how a
    # genuine crash looks - two different problems wearing the same face.
    Start-Process -FilePath "powershell.exe" -Verb RunAs -WindowStyle Hidden -Wait -ErrorAction Stop -ArgumentList {arglist}
}} catch {{
    Set-Content -LiteralPath {out} -Value "UAC_DENIED" -Encoding UTF8
}}
"#,
        // Both quoted the same way as in `wrapped`, and for the same reason. The
        // ArgumentList is ONE PowerShell string (passing an array joins the
        // elements with spaces and no quoting, which breaks a user whose name
        // has a space); the inner double quotes around the path are what
        // powershell.exe itself needs, and Windows paths cannot contain `"`.
        arglist = ps_single_quote(&format!(
            "-NoProfile -ExecutionPolicy Bypass -File \"{}\"",
            payload.to_string_lossy()
        )),
        out = ps_single_quote(&outfile.to_string_lossy())
    );

    let launcher_path = dir.join(format!("elev_run_{tag}.ps1"));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&launcher_path)
    {
        Ok(mut f) => {
            let _ = f.write_all(launcher.as_bytes());
        }
        Err(_) => {
            let _ = std::fs::remove_file(&payload);
            return "Error: could not write the temporary script for the elevated step.".to_string();
        }
    }
    let launcher_str = launcher_path.to_string_lossy().to_string();
    // The return value tells a timeout apart from a denied UAC prompt: both
    // leave `output` empty below.
    let launcher_out = run_process_timeout(
        "powershell.exe",
        &["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &launcher_str],
        ELEVATED_TIMEOUT,
    );
    let timed_out = launcher_out.starts_with("Timed out after");

    let raw = std::fs::read_to_string(&outfile).unwrap_or_default();
    // On a timeout the launcher is dead but the ELEVATED CHILD is not - it runs
    // at a higher integrity level and cannot be killed from here. Deleting
    // these three files while it is still using them would pull the payload
    // out from under a script that is still reading it, and throw away the
    // output file the child is about to write. Leave them: `script_scratch_dir`
    // sweeps anything older than a day.
    if !timed_out {
        let _ = std::fs::remove_file(&payload);
        let _ = std::fs::remove_file(&launcher_path);
        let _ = std::fs::remove_file(&outfile);
    }
    // Out-File -Encoding utf8 writes a BOM on Windows PowerShell 5.1; left in
    // place it would sit in front of the first character and defeat every
    // "does this start with Done." test downstream.
    let output = raw.trim_start_matches('\u{feff}').trim().to_string();

    if output == "UAC_DENIED" {
        return format!(
            "{} Administrator permission was denied, so nothing was changed.",
            UAC_DENIED_PREFIX
        );
    }
    if output.is_empty() {
        if timed_out {
            // Empty BECAUSE the child has not finished, not because it never
            // started. Saying "nothing was changed" here would be the exact
            // opposite of the truth for the firewall path, which deletes rules
            // before recreating them.
            return format!(
                "{} The administrator step has taken more than {}s, so this window stopped waiting for it. \
                 It is still running in the background and will finish on its own - a firewall rebuild on a \
                 machine with many rules can take a few minutes. Give it a minute before trying again; \
                 running it a second time now would start the whole thing over.",
                ELEVATED_STILL_RUNNING_PREFIX,
                ELEVATED_TIMEOUT.as_secs()
            );
        }
        // No file at all and no timeout: the elevated child never got far
        // enough to write one, which means the prompt was dismissed.
        return format!(
            "{} Administrator permission was not granted, so nothing was changed.",
            UAC_DENIED_PREFIX
        );
    }
    output
}

// ---------- Device registry ----------
// Persists the list of paired devices as JSON in the app's data dir (e.g.
// %APPDATA%/com.awadb.assistant on Windows). `id` is a stable identifier
// independent of `host_port`, since the phone's Wireless debugging port
// changes randomly after every reboot - the host_port on file gets updated
// in place (re-pair) while the id and alias stay put.
#[derive(Serialize, Deserialize, Clone)]
struct Device {
    id: u64,
    alias: String,
    /// The phone's wireless address, `ip:port`.
    ///
    /// **Empty means "added over the cable, no wireless address known".** A USB
    /// device needs no pairing and has no ip:port - `serial` is its whole
    /// identity, and `adb -s <serial>` is how you reach it. Empty is not a
    /// broken entry: it is a device that simply has not been paired for Wi-Fi,
    /// and it fills in by itself the day it is - or the day the phone shows
    /// up over Wi-Fi and reports this serial (the app on its status poll,
    /// `reconcile_devices` at start, and the watcher when it reconnects one,
    /// all write the address they confirmed). From then on
    /// the row is a Wi-Fi row too: USB still wins whenever the cable is in.
    host_port: String,
    /// The phone's hardware serial (from its `_adb-tls-connect` mDNS name,
    /// e.g. `adb-R5CX12ABCDE-...` -> `R5CX12ABCDE`). This is the stable device
    /// identity across sessions - unlike host_port (port changes every session)
    /// and even IP (can change via DHCP). `#[serde(default)]` keeps older
    /// devices.json files (written before this field existed) loadable; they
    /// get a serial stamped on the next pairing.
    #[serde(default)]
    serial: Option<String>,
    /// **"This is a cable phone: do not remember a Wi-Fi address for it."**
    ///
    /// Set by "Forget Wi-Fi address" in the row menu, and cleared when the row
    /// is given an address again: "Re-pair this device", or any other explicit
    /// `update_device`/`add_device` that carries one AND lands on this row.
    /// (Pairing from the wizard lands on this row when it learns the phone's
    /// serial, which it normally does; a pairing that does not learn one has
    /// no way to tell this row from another phone and creates a new row.)
    ///
    /// While it is on, neither the app's status poll nor the watcher writes an
    /// address into this row; the watcher does not dial the phone by mDNS or
    /// count it as missing; and `reconcile_devices` drops its wireless
    /// transport when it finds one, unless a mirror window is using it.
    ///
    /// **A transport adb itself opened under an mDNS NAME cannot be dropped by
    /// anyone.** `adb disconnect` (adb 37.0.0) matches a transport by the
    /// serial adb LISTS it under and appends
    /// `:5555` when no port is given, so neither the name nor the `ip:port`
    /// behind it ends that transport. The row menu says so instead of
    /// promising otherwise, and names the one thing that does work: turning
    /// off wireless debugging on the phone.
    ///
    /// Why a flag and not just an empty `host_port`: an empty address fills
    /// in by itself the first time the phone is recognised over
    /// Wi-Fi, which is what we want for a phone that was simply added over USB
    /// and is otherwise wireless. "Forget" has to mean something stronger than
    /// "empty", or the next poll undoes it.
    ///
    /// `skip_serializing_if` keeps it out of devices.json for every row that
    /// does not use it.
    #[serde(default, deserialize_with = "loose_bool", skip_serializing_if = "is_false")]
    cable_only: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A boolean that also accepts what a person types by hand.
///
/// `read_devices` turns ANY parse error into an empty device list, so a single
/// `"cable_only": "true"` typed into devices.json would make every phone
/// disappear from the app - and the app would then write that empty list back.
/// Taking `true`/`false` as strings, and 0/1, costs four lines and removes a
/// way to lose the registry. Anything else still fails, loudly, rather than
/// guessing.
fn loose_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Bool(value) => Ok(value),
        serde_json::Value::Number(value) => match value.as_i64() {
            Some(number) => Ok(number != 0),
            // A float, or an integer past i64: not a yes/no answer, and
            // guessing "no" would silently ignore what the file says.
            None => Err(D::Error::custom(format!("not a yes/no value: {value}"))),
        },
        serde_json::Value::String(value) => match value.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            other => Err(D::Error::custom(format!("not a yes/no value: {other}"))),
        },
        serde_json::Value::Null => Ok(false),
        other => Err(D::Error::custom(format!("not a yes/no value: {other}"))),
    }
}

/// A plain `<ip>:<port>` - the only shape `adb disconnect` understands.
///
/// Deliberately stricter than `is_wireless_transport`, which also answers yes
/// for an mDNS transport NAME: handing one of those to `adb disconnect` is not
/// a disconnect, it is a typo with a network call attached.
fn is_ip_port(address: &str) -> bool {
    let mut parts = address.rsplitn(2, ':');
    let port = match parts.next() {
        Some(p) => p,
        None => return false,
    };
    let host = match parts.next() {
        Some(h) => h,
        None => return false,
    };
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let octets: Vec<&str> = host.split('.').collect();
    octets.len() == 4
        && octets
            .iter()
            .all(|o| !o.is_empty() && o.len() <= 3 && o.bytes().all(|b| b.is_ascii_digit()))
}

fn devices_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("devices.json"))
}

/// Set when devices.json exists but cannot be read as a device list.
///
/// Why this flag exists: `read_devices` answers with an empty list for both
/// "no file yet" and "file I cannot parse", and every writer then treats that
/// empty list as the truth - one mistyped character in the file and the next
/// save wipes every phone. The flag keeps the two apart: the app shows no
/// devices (it genuinely cannot read them) but nothing overwrites the file
/// until it parses again, so the person can fix their typo and get their
/// phones back.
fn registry_unreadable() -> &'static std::sync::atomic::AtomicBool {
    static FLAG: OnceLock<std::sync::atomic::AtomicBool> = OnceLock::new();
    FLAG.get_or_init(|| std::sync::atomic::AtomicBool::new(false))
}

fn read_devices(app: &tauri::AppHandle) -> Vec<Device> {
    let path = match devices_file_path(app) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        // No file is not a problem: the first pairing writes one.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            registry_unreadable().store(false, std::sync::atomic::Ordering::Relaxed);
            return Vec::new();
        }
        // Anything else - a sharing violation while the watcher swaps the file,
        // a permission problem - is "I could not read it", not "there are no
        // phones": treated as an empty registry, they would let an empty list
        // be written back.
        Err(error) => {
            if !registry_unreadable().swap(true, std::sync::atomic::Ordering::Relaxed) {
                log_activity(&format!(
                    "devices.json could not be opened ({error}); the device list stays empty and will NOT be overwritten until it can be read again."
                ));
            }
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<Device>>(&contents) {
        Ok(devices) => {
            registry_unreadable().store(false, std::sync::atomic::Ordering::Relaxed);
            devices
        }
        Err(error) => {
            if !registry_unreadable().swap(true, std::sync::atomic::Ordering::Relaxed) {
                log_activity(&format!(
                    "devices.json could not be read ({error}); the device list stays empty and will NOT be overwritten until the file parses again."
                ));
            }
            Vec::new()
        }
    }
}

fn write_devices(app: &tauri::AppHandle, devices: &[Device]) -> Result<(), String> {
    // Refuses rather than overwrite a file it could not read - see
    // registry_unreadable.
    if registry_unreadable().load(std::sync::atomic::Ordering::Relaxed) {
        return Err(
            "Mirryl cannot read devices.json, so it will not overwrite it. Fix or remove that file and reopen Mirryl."
                .to_string(),
        );
    }
    let path = devices_file_path(app)?;
    let json = serde_json::to_string_pretty(devices).map_err(|e| e.to_string())?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Whether a mirror window is open for the phone with this serial.
///
/// The registry of open windows is keyed by the device id as a string (the
/// frontend's `openMirrors`), so the row has to be found first.
fn mirror_open_for_serial(devices: &[Device], serial: &str) -> bool {
    let id = match devices
        .iter()
        .find(|d| d.serial.as_deref() == Some(serial))
    {
        Some(d) => d.id.to_string(),
        None => return false,
    };
    match open_mirrors().lock() {
        Ok(open) => open.contains_key(&id),
        Err(_) => false,
    }
}

/// Forgets the Wi-Fi address of one row and marks it cable-only.
///
/// What the person asked for is "stop chasing this phone on the network", so
/// three things happen together: the saved address goes, the row is marked so
/// nothing writes one back ([`Device::cable_only`]), and the live wireless
/// transport - if the phone is on one right now - is dropped, because leaving
/// it connected would keep the row green over the very link that was just
/// forgotten. The cable is untouched: `adb disconnect` only speaks to
/// `ip:port` transports.
///
/// `live` is what the row is connected through at this moment, as the
/// frontend sees it; anything that is not a plain `ip:port` is ignored rather
/// than handed to adb.
#[tauri::command]
fn forget_wifi_address(
    app: tauri::AppHandle,
    id: u64,
    live: Option<String>,
) -> Result<Vec<Device>, String> {
    let mut devices = read_devices(&app);
    let mut addresses: Vec<String> = Vec::new();
    match devices.iter_mut().find(|d| d.id == id) {
        Some(d) => {
            if is_ip_port(&d.host_port) {
                addresses.push(d.host_port.clone());
            }
            d.host_port = String::new();
            d.cable_only = true;
        }
        None => return Err("No such device.".to_string()),
    }
    if let Some(live) = live {
        let live = live.trim().to_string();
        if is_ip_port(&live) && !addresses.contains(&live) {
            addresses.push(live);
        }
    }
    write_devices(&app, &devices)?;
    for address in addresses {
        let _ = run_process(&adb_path(), &["disconnect", &address]);
    }
    Ok(devices)
}

#[tauri::command]
fn list_devices(app: tauri::AppHandle) -> Vec<Device> {
    read_devices(&app)
}

/// Insert-or-update a device, keyed by the phone's stable identity. A phone's
/// Wireless-debugging PORT changes every session (and its IP can change via
/// DHCP), so the same physical phone otherwise shows up as a brand-new row.
/// Identity preference: (1) hardware `serial` when known, else (2) the IP.
/// A match updates the existing entry in place - keeping its id and any
/// user-set alias, stamping the serial if it was missing (migrating older
/// IP-only entries) - and collapses any other rows that match the same phone;
/// no match appends a new device.
fn upsert_device(devices: &mut Vec<Device>, alias: String, host_port: String, serial: Option<String>) {
    let ip = host_port.split(':').next().unwrap_or("").to_string();

    let ip_matches = |d: &Device| !ip.is_empty() && d.host_port.split(':').next() == Some(ip.as_str());

    // Does an existing device refer to the same physical phone as this pairing?
    // When BOTH sides have a serial, the serial is authoritative - two phones
    // that momentarily share an IP (DHCP reuse) must NOT be merged. IP is only
    // the fallback when a serial is unknown on one side.
    let same_phone = |d: &Device| -> bool {
        match (&serial, &d.serial) {
            (Some(s), Some(ds)) => s == ds,
            _ => ip_matches(d),
        }
    };

    match devices.iter().position(same_phone) {
        Some(first) => {
            // An empty host_port means "added over USB, no wireless address
            // known". It must never overwrite one we already have: plugging a
            // cable into a phone that is already registered over Wi-Fi would
            // otherwise wipe the ip:port that lets it work without the cable.
            if !host_port.is_empty() {
                devices[first].host_port = host_port;
                // Pairing this phone over Wi-Fi is the plainest way of saying
                // "remember its address again".
                devices[first].cable_only = false;
            }
            if serial.is_some() {
                devices[first].serial = serial.clone();
            }
            // Collapse any other rows for the same phone (e.g. IP-only rows
            // saved before the serial was known), keeping the updated entry.
            let keep_id = devices[first].id;
            devices.retain(|d| d.id == keep_id || !same_phone(d));
        }
        None => {
            let next_id = devices.iter().map(|d| d.id).max().unwrap_or(0) + 1;
            devices.push(Device { id: next_id, alias, host_port, serial, cable_only: false });
        }
    }
}

#[tauri::command]
fn add_device(
    app: tauri::AppHandle,
    alias: String,
    host_port: String,
    serial: Option<String>,
) -> Result<Vec<Device>, String> {
    let mut devices = read_devices(&app);
    upsert_device(&mut devices, alias, host_port, serial);
    write_devices(&app, &devices)?;
    Ok(devices)
}

#[tauri::command]
fn update_device(
    app: tauri::AppHandle,
    id: u64,
    alias: Option<String>,
    host_port: Option<String>,
    serial: Option<String>,
) -> Result<Vec<Device>, String> {
    let mut devices = read_devices(&app);
    if let Some(d) = devices.iter_mut().find(|d| d.id == id) {
        if let Some(a) = alias {
            if a != d.alias {
                rename_tools_output_dir(&d.alias, &a);
            }
            d.alias = a;
        }
        if let Some(hp) = host_port {
            // An address the user asked for, so the row wants one again.
            if !hp.trim().is_empty() {
                d.cable_only = false;
            }
            d.host_port = hp;
        }
        // Only ever fills a gap, never overwrites: the serial is the stable
        // identity, and a wrong one would merge two different phones. A row
        // saved without one arrives here with None, and this is what heals it.
        if d.serial.is_none() {
            if let Some(learned) = serial.filter(|value| !value.trim().is_empty()) {
                d.serial = Some(learned);
            }
        }
    }
    write_devices(&app, &devices)?;
    Ok(devices)
}

#[tauri::command]
fn remove_device(app: tauri::AppHandle, id: u64) -> Result<Vec<Device>, String> {
    let mut devices = read_devices(&app);
    // Drop the underlying adb connection for the removed device, so deleting it
    // from the app also clears it from `adb devices` (which the LIVE STATUS
    // reflects). Without this, adb keeps the wireless connection cached and the
    // deleted device lingers as a phantom. Only ip:port (wireless) entries are
    // disconnectable; USB serials have no ':' and are left alone.
    if let Some(removed) = devices.iter().find(|d| d.id == id) {
        if removed.host_port.contains(':') {
            let _ = run_process(&adb_path(), &["disconnect", &removed.host_port]);
        }
    }
    devices.retain(|d| d.id != id);
    write_devices(&app, &devices)?;
    Ok(devices)
}

/// The states adb prints after a transport name in `adb devices`.
const ADB_STATES: [&str; 10] = [
    "device",
    "offline",
    "unauthorized",
    "authorizing",
    "connecting",
    "bootloader",
    "recovery",
    "sideload",
    "host",
    "no permissions",
];

/// One line of `adb devices` / `adb devices -l`, split into (transport, state).
///
/// The transport name is NOT one whitespace-free token. When a phone
/// re-announces Wireless debugging while its old announcement is still alive,
/// mDNS renames the service and the rename puts a SPACE inside it:
/// `adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp`. Splitting on whitespace
/// would read `(2)._adb-tls-connect._tcp` as the state, and a connected phone
/// would look absent. Plain `adb devices` separates the columns with a tab but
/// `-l` pads with spaces, so neither form can be split blindly: find the state
/// keyword and take everything before it - the same rule as `parseAdbLine` in
/// main.js. The header and adb's own chatter
/// ("List of devices attached", "* daemon started") give None.
fn parse_adb_device_line(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    let mut start = 0;
    while let Some(offset) = line[start..].find(char::is_whitespace) {
        let gap = start + offset;
        let rest = line[gap..].trim_start();
        for state in ADB_STATES {
            if let Some(after) = rest.strip_prefix(state) {
                if after.is_empty() || after.starts_with(char::is_whitespace) {
                    let name = line[..gap].trim_end();
                    if !name.is_empty() {
                        return Some((name, state));
                    }
                }
            }
        }
        start = line.len() - rest.len();
    }
    None
}

/// A line of three columns whose FIRST may contain spaces, read from the
/// right: `adb mdns services` (`<instance>\t<service>\t<ip:port>`). The
/// service and the address never contain whitespace; the instance does after
/// an mDNS rename (`adb-R5CX12ABCDE-62Sel0 (2)`), and splitting on whitespace
/// would take `(2)` for the service and lose the phone's current address.
fn split_last_two_fields(line: &str) -> Option<(&str, &str, &str)> {
    let (rest, last) = line.trim().rsplit_once(char::is_whitespace)?;
    let (first, middle) = rest.trim_end().rsplit_once(char::is_whitespace)?;
    let first = first.trim_end();
    if first.is_empty() || middle.is_empty() || last.is_empty() {
        return None;
    }
    Some((first, middle, last))
}

/// The ip:port transports `adb devices` lists in the `device` state only -
/// the ones that can answer a question. `offline`, `connecting`,
/// `authorizing` and `unauthorized` are what a transport looks like for a
/// second or two right after `adb connect`; asking one of those anything
/// just fails.
fn parse_ready_wireless_devices(output: &str) -> Vec<String> {
    let re = regex::Regex::new(r"(?m)^(\d{1,3}(?:\.\d{1,3}){3}:\d+)\s+device(?:\s|$)").expect("valid regex");
    re.captures_iter(output).map(|c| c[1].to_string()).collect()
}

/// Maps each mDNS transport to the address it currently lives at.
///
/// `adb mdns services` prints `<name>\t<service>\t<ip:port>`, and the transport
/// name adb reports in `adb devices` is `<name>.<service>`. Joining them is the
/// only way to tell that `adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp` and
/// `192.168.0.4` are the same phone.
///
/// This is what a phone needs after a REBOOT: it comes back under its mDNS name
/// only, with a brand-new port, so neither the saved ip:port nor its IP appears
/// anywhere in `adb devices`. Without this the app cannot recognise a device it
/// is, at that very moment, mirroring.
fn parse_mdns_transports(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter_map(|line| {
            let (instance, service, address) = split_last_two_fields(line)?;
            // Only the connect service. `_adb-tls-pairing` is a different port
            // that refuses connections, and matching it would hand out an
            // address nothing can talk to.
            if !service.contains("_adb-tls-connect") {
                return None;
            }
            if !address.contains(':') {
                return None;
            }
            Some((format!("{instance}.{service}"), address.to_string()))
        })
        .collect()
}

#[tauri::command]
async fn mdns_transports() -> Vec<(String, String)> {
    parse_mdns_transports(&run_process(&adb_path(), &["mdns", "services"]))
}

/// The phone's stable hardware serial, read straight off the live connection -
/// works over any transport (USB, `ip:port`, mDNS name), unlike `get-serialno`
/// which for a manually `adb connect`-ed `ip:port` just echoes that same
/// address back rather than the actual hardware id.
///
/// Exists because QR pairing already knows the serial (learned during mDNS
/// discovery) but the manual IP:Port + code path never does - `finishSetup`
/// calls this to fill that gap before saving, so a phone added by USB and
/// then paired manually over Wi-Fi merges into its existing row instead of
/// creating a second one for the same physical device.
#[tauri::command]
async fn device_serial(host_port: String) -> Option<String> {
    let host_port = host_port.trim();
    if host_port.is_empty() {
        return None;
    }
    transport_serial(host_port, PROCESS_TIMEOUT)
}

/// The serial the phone behind `host_port` reports over the adb channel. This
/// is the only identity that is not chosen by whoever is on the Wi-Fi: an mDNS
/// name is free text (see qr_pairing::discover_connect_for), the answer to
/// `getprop` comes from the device we actually connected to.
fn transport_serial(host_port: &str, timeout: Duration) -> Option<String> {
    let adb = adb_path();
    let output = run_process_timeout(
        &adb,
        &["-s", host_port, "shell", "getprop", "ro.serialno"],
        timeout,
    );
    let candidate = output.trim();
    // A real serial is one bare token - anything with whitespace in it is an
    // error message (unreachable device, adb hiccup), not a serial to trust.
    if candidate.is_empty() || candidate.contains(char::is_whitespace) {
        None
    } else {
        Some(candidate.to_string())
    }
}

/// Reconcile adb's connection table with the app's registry: disconnect any
/// wireless device adb has connected that is NOT in devices.json. Clears
/// phantoms left by deleted devices or by connections made outside the app, so
/// the LIVE STATUS matches the managed device list. Safe to call only when no
/// pairing/connect is mid-flight (the frontend calls it on the Device Manager,
/// never during the wizard, so a just-connected-not-yet-saved phone is never
/// dropped). Returns the registry, with the address of any registered phone
/// found at a new one already updated.
#[tauri::command]
async fn reconcile_devices(app: tauri::AppHandle) -> Vec<Device> {
    let devices = read_devices(&app);
    // An empty list because the file could not be READ is not an empty list.
    // Without this, one stray comma in devices.json would turn opening Mirryl
    // into "disconnect every phone on Wi-Fi": no row owns any IP, so every live
    // transport looks like a phantom - and the open-mirror guard below cannot
    // save them either, because it looks for the row in this same empty list.
    if registry_unreadable().load(std::sync::atomic::Ordering::Relaxed) {
        return devices;
    }
    // Match by IP, not exact ip:port: the phone's port changes across sessions,
    // so a legit reconnection on a NEW port (same IP as a registered device)
    // must NOT be treated as a phantom.
    let known_ips: std::collections::HashSet<String> = devices
        .iter()
        .filter_map(|d| d.host_port.split(':').next().map(str::to_owned))
        .collect();
    // (serial, address) of our own phones found at an address not on file.
    let mut moved: Vec<(String, String)> = Vec::new();
    // Only transports in the `device` state: one still `connecting` or
    // `offline` a second after the watcher dialled it cannot answer yet, and
    // must not be dropped for that.
    let listing = run_process(&adb_path(), &["devices"]);
    for connected in parse_ready_wireless_devices(&listing) {
        let ip = connected.split(':').next().unwrap_or("");
        if known_ips.contains(ip) {
            continue;
        }
        // An IP that is on no row is not yet a phantom: it may be one of our
        // phones at a NEW address - DHCP moved it, or the watcher found it by
        // mDNS while this app was closed - and dropping it could leave that
        // phone off until someone reconnects it by hand. So ask the phone who
        // it is first - the same question the watcher asks - and decide three
        // ways: a serial on
        // one of our rows means the phone moved, and its new address is
        // written down so the row and the watcher use it; a serial on no row
        // is a phantom and is dropped; no answer at all (slow phone, adb
        // hiccup) means nothing is known yet, and nothing is done - the next
        // start asks again. Short timeout: this runs before the list shows.
        match transport_serial(&connected, Duration::from_secs(5)) {
            Some(reported) => {
                // A cable-only row is ours, but "Forget Wi-Fi address" said to
                // stop keeping it on the network, so its wireless transport is
                // dropped like any other address that belongs to no row. The
                // watcher does not dial it either, so this does not turn into
                // a disconnect/redial loop.
                let ours = devices
                    .iter()
                    .any(|d| d.serial.as_deref() == Some(reported.as_str()) && !d.cable_only);
                if ours {
                    moved.push((reported, connected.clone()));
                } else if mirror_open_for_serial(&devices, &reported) {
                    // Something is being watched over this link right now.
                    // Dropping it would kill the picture on screen with no
                    // explanation - and this runs unasked, every time the
                    // Device Manager opens. Leave it; the next start, with the
                    // window closed, does the tidying.
                } else {
                    let _ = run_process(&adb_path(), &["disconnect", &connected]);
                }
            }
            None => {}
        }
    }
    if moved.is_empty() {
        return devices;
    }
    // Re-read before writing: the questions above can take seconds, and the
    // watcher may have written an address of its own in the meantime. Only
    // the rows that moved are touched, on a fresh copy.
    let mut fresh = read_devices(&app);
    let mut changed = false;
    for (serial, address) in moved {
        // `!cable_only` again on the fresh copy, not only on the one read at
        // the start: asking each unknown transport who it is takes up to five
        // seconds per address, and "Forget Wi-Fi address" can land inside that
        // window - writing the address back would undo it silently.
        if let Some(row) = fresh
            .iter_mut()
            .find(|d| d.serial.as_deref() == Some(serial.as_str()) && !d.cable_only)
        {
            if row.host_port != address {
                row.host_port = address;
                changed = true;
            }
        }
    }
    if changed {
        let _ = write_devices(&app, &fresh);
    }
    fresh
}

// ---------- Theme preference ----------
// A manual choice, persisted across restarts and reboots. Defaults to
// "light" (not the OS setting) until the user picks something themselves.
#[derive(Serialize, Deserialize)]
struct ThemePref {
    theme: String,
}

fn theme_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("theme.json"))
}

#[tauri::command]
fn get_theme(app: tauri::AppHandle) -> String {
    let path = match theme_file_path(&app) {
        Ok(p) => p,
        Err(_) => return "light".to_string(),
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<ThemePref>(&contents)
            .map(|p| p.theme)
            .unwrap_or_else(|_| "light".to_string()),
        Err(_) => "light".to_string(),
    }
}

// ---------- Keyboard shortcuts ----------
// A map of action id -> shortcut ("panel_power" -> "Ctrl+O"), saved next to the
// other preferences and read by the MIRRORING ENGINE, which is a different
// process with its own window. That is the whole reason this is a file instead
// of something held in memory: the settings screen lives in this app, and the
// keys are pressed over there.
//
// Rust deliberately keeps no list of actions or defaults. The names, the
// labels and the default shortcuts belong to the settings screen (main.js,
// SHORTCUT_ACTIONS), which writes the COMPLETE map every time. Duplicating that
// table here would be two lists to keep in step, and they would drift.

fn keybindings_file_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("keybindings.json"))
}

/// The same file as above, worked out without an `AppHandle`.
///
/// `start_mirror_engine` has no handle to ask, and this is the path it hands
/// the engine. Built the way the window-position file next to it already is;
/// both land in the app-data folder that `app_data_dir()` returns for this
/// identifier - checked, not assumed.
fn keybindings_path_for_engine() -> Option<PathBuf> {
    roaming_app_dir().map(|dir| dir.join("keybindings.json"))
}

/// Where the engine remembers "don't show the game-mode hint again".
///
/// A plain marker file, not JSON, on purpose - the engine writes this one
/// itself (the app never touches it), same as MIRRYL_WINDOW_STATE_FILE. The
/// content does not matter, only whether the file exists.
fn game_hint_path_for_engine() -> Option<PathBuf> {
    roaming_app_dir().map(|dir| dir.join("game-hint-dismissed.txt"))
}

#[tauri::command]
fn get_keybindings(app: tauri::AppHandle) -> String {
    // "{}" rather than an error when there is nothing saved yet: the settings
    // screen fills the gaps from its own defaults, so a first run and a missing
    // file are the same situation and need no special case up there.
    let Ok(path) = keybindings_file_path(&app) else {
        return "{}".to_string();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) if serde_json::from_str::<serde_json::Value>(&contents).is_ok() => contents,
        _ => "{}".to_string(),
    }
}

#[tauri::command]
fn set_keybindings(app: tauri::AppHandle, bindings: String) -> Result<(), String> {
    // Checked here and not only in the page: this file is read by another
    // process that has no way to ask what was meant by a broken one.
    let parsed: serde_json::Value =
        serde_json::from_str(&bindings).map_err(|error| format!("shortcuts are not valid JSON: {error}"))?;
    let Some(map) = parsed.as_object() else {
        return Err("shortcuts must be an object of action -> shortcut".to_string());
    };
    if map.values().any(|value| !value.is_string()) {
        return Err("every shortcut must be text".to_string());
    }
    let path = keybindings_file_path(&app)?;
    // Written to one side and moved into place, like the theme: a half-written
    // file here would reach a mirror window that cannot ask what was meant.
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &bindings).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- The native title bar ----------
// Windows paints the caption (icon, "Mirryl", minimise/maximise/close) dark by
// default and takes no notice of what the app itself is doing, so the light
// theme would get a black bar over a white interface.
//
// Painted with DwmSetWindowAttribute rather than by turning decorations off and
// drawing our own: the real caption keeps the native buttons, snap layouts, and
// double-click-to-maximise for free, and every one of those is work to rebuild
// and easy to get subtly wrong.
//
// The colours follow the APP's theme button, not the Windows theme - the two
// disagree on purpose, since the whole point is matching the interface below.

/// `DwmSetWindowAttribute` takes a COLORREF, which is `0x00BBGGRR` - the bytes
/// run the OPPOSITE way round to an HTML colour. Written out here rather than
/// hand-swapped in the table below, because a silently reversed colour is
/// exactly the bug this API is famous for.
#[cfg(windows)]
const fn colorref(rgb: u32) -> u32 {
    let red = (rgb >> 16) & 0xff;
    let green = (rgb >> 8) & 0xff;
    let blue = rgb & 0xff;
    (blue << 16) | (green << 8) | red
}

/// Paint the caption to match the app's theme. Best effort by design.
///
/// The three attributes are Windows 11 (build 22000+) only. On Windows 10 the
/// calls return an error and the caption stays default - which is the correct
/// outcome, so nothing here reports failure upwards. A title bar that could not
/// be recoloured must never be able to stop the app from starting.
#[cfg(windows)]
fn paint_caption(window: &tauri::WebviewWindow, theme: &str) {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;

    const DWMWA_BORDER_COLOR: u32 = 34;
    const DWMWA_CAPTION_COLOR: u32 = 35;
    const DWMWA_TEXT_COLOR: u32 = 36;

    let dark = theme == "dark";
    // Straight from the app's palette, so the bar and the page below it are the
    // same colour rather than merely similar.
    let caption = if dark { 0x070914 } else { 0xf3f5ff };
    let text = if dark { 0xf7f8ff } else { 0x101426 };
    // In dark the border matching the caption is what makes the window read as
    // one solid shape. In light it cannot: #f3f5ff on a light desktop leaves the
    // window with no visible edge at all, so light borrows the palette's border
    // tone instead - still quiet, but the window has an outline.
    let border = if dark { caption } else { 0xd6dcf5 };

    let Ok(handle) = window.hwnd() else {
        return;
    };
    let hwnd = handle.0 as HWND;
    for (attribute, value) in [
        (DWMWA_CAPTION_COLOR, colorref(caption)),
        (DWMWA_TEXT_COLOR, colorref(text)),
        (DWMWA_BORDER_COLOR, colorref(border)),
    ] {
        // Errors ignored on purpose - see the note above about Windows 10.
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                attribute,
                std::ptr::addr_of!(value).cast(),
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

#[cfg(not(windows))]
fn paint_caption(_window: &tauri::WebviewWindow, _theme: &str) {}

#[tauri::command]
fn set_theme(app: tauri::AppHandle, theme: String) -> Result<(), String> {
    // Repaint before saving: the button should feel instant, and a failed write
    // is not a reason to leave the bar disagreeing with the page.
    if let Some(window) = app.get_webview_window("main") {
        paint_caption(&window, &theme);
    }
    let path = theme_file_path(&app)?;
    let json = serde_json::to_string(&ThemePref { theme }).map_err(|e| e.to_string())?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, json).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp_path, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ---------- Android seal ----------
// The phone's own app keeps re-enabling Wireless debugging on its own schedule
// (Alarm/Job/Boot receivers), whatever any PC is doing right now. It only does
// so while a Mirryl PC has stamped it recently (this seal) or matriculated it
// (`matriculation.rs`, the permanent, per-PC-signed binding): a phone nobody's
// Mirryl has touched should not keep a debugging port open on its own. The
// stamp is pushed on every connection; the phone side is in WifiAdbHelper.java.
//
// What it proves is only "a Mirryl PC touched this phone recently".
// SEAL_PEPPER lives in this binary AND in the APK's WifiAdbHelper.java (it has
// to, to verify), so anyone can forge a stamp by hand with plain adb - which
// is fine: the stamp is not a lock, matriculation is the real binding.
const SEAL_REMOTE_DIR: &str = "/sdcard/Android/data/com.mirryl.app/files/";
// Deliberately unremarkable name - not "seal" or "license", so it doesn't
// jump out of a directory listing or a decompile's string table.
const SEAL_REMOTE_NAME: &str = ".mstate";
// MUST match WifiAdbHelper.SEAL_PEPPER (Android) and Send-Seal's $pepper
// (watcher.ps1) exactly - all three compute the same hash independently.
const SEAL_PEPPER: &str = "mirryl-seal-v1-8f2c";

fn seal_json(now: u64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{SEAL_PEPPER}:{now}").as_bytes());
    let v = hex::encode(hasher.finalize());
    format!("{{\"v\":\"{v}\",\"ts\":{now}}}")
}

/// Writes a short-lived file that is about to be `adb push`ed - the seal, the
/// matriculation certificate - somewhere only this user can reach, under a
/// random name, created exclusively: in %TEMP% under a predictable name, on a
/// machine where another account has Modify on %TEMP%, the content could be
/// swapped between the write and the push.
/// The caller removes the file after the push.
fn stage_private_file(kind: &str, content: &str) -> Option<PathBuf> {
    let dir = roaming_app_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let mut random = [0u8; 8];
    {
        use rand::RngCore;
        rand::rngs::OsRng.fill_bytes(&mut random);
    }
    let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let path = dir.join(format!("mirryl-{kind}-{name}.json"));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| {
            use std::io::Write;
            file.write_all(content.as_bytes())
        });
    if written.is_err() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

/// Best-effort stamp so the phone's own keepalive keeps working. Never blocks
/// or fails the caller: a phone that ends up unstamped just needs another
/// connection from Mirryl within the Android side's max-age window, it
/// does not break the connection this push rides along on.
fn push_seal(adb: &str, target: &str) {
    let now = now_secs();
    let content = seal_json(now);
    let Some(path) = stage_private_file("seal", &content) else {
        return;
    };
    let remote = format!("{SEAL_REMOTE_DIR}{SEAL_REMOTE_NAME}");
    let _ = run_process(adb, &["-s", target, "push", &path.to_string_lossy(), &remote]);
    let _ = std::fs::remove_file(&path);
}

/// Whether `target` (a USB serial or a wireless address) belongs to a phone
/// in the list. A phone mirrored over the cable from the offer row has no
/// row, and a phone without a row has nothing to matriculate: matriculation is about
/// reconnecting a saved phone on its own.
fn is_listed(devices: &[Device], target: &str) -> bool {
    devices.iter().any(|d| {
        d.serial.as_deref().is_some_and(|s| s == target || target.contains(s))
            || (!d.host_port.is_empty() && d.host_port == target)
    })
}

/// A wireless transport is `ip:port` or the mDNS name
/// (`adb-<SERIAL>-xxxx._adb-tls-connect._tcp`). A bare hardware serial - the
/// USB case - has neither a colon nor the suffix.
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (watcher, wireless reconnect).
fn is_wireless_transport(address: &str) -> bool {
    // `._tcp` is tested with `contains`, not `ends_with`, and `adb-` is tested
    // too: a renamed mDNS service is `adb-<SERIAL>-xxxx (2)._adb-tls-connect._tcp`
    // - the rename can land anywhere.
    address.contains(':') || address.contains("._tcp") || address.starts_with("adb-")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The version the About screen shows, read from the crate so it can never
/// disagree with the build.
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------- Commands ----------

#[tauri::command]
async fn pair(host_port: String, code: String) -> String {
    // Pairing exists only for wireless debugging. Over USB there is nothing to
    // pair.
    let host_port = host_port.trim();
    let code = code.trim();
    if host_port.is_empty() || code.is_empty() {
        return "Enter both the pairing IP:Port and the 6-digit code.".to_string();
    }
    run_process(&adb_path(), &["pair", host_port, code])
}

#[derive(Serialize)]
struct ConnectResult {
    connect: String,
    grant: String,
    /// The ip:port we actually ended up connecting to. May differ from the one
    /// passed in when the saved port was stale and we re-resolved a fresh one -
    /// the frontend persists this back so next time it's current.
    host_port: String,
    /// Something the user should read that is not an error: the identity
    /// notice of `matriculation_notice`. Empty when there is nothing to say.
    #[serde(default)]
    notice: String,
    /// What the phone has of the Mirryl phone app, as `PhoneAppState::as_str`
    /// spells it ("ready", "missing", "outdated", "unknown"), or "" when the
    /// connection never got far enough to ask. "missing" means nothing was
    /// granted or matriculated: the wizard offers to install the app, then
    /// connects again.
    #[serde(default)]
    phone_app: String,
}

/// Whether an `adb connect` output indicates success ("connected to ..." or
/// "already connected ..."), tolerating adb's varied phrasing.
fn connect_succeeded(output: &str) -> bool {
    let l = output.to_lowercase();
    (l.contains("connected to") || l.contains("already connected"))
        && !l.contains("failed")
        && !l.contains("cannot")
        && !l.contains("unable")
}

/// Resolve the phone's CURRENT `_adb-tls-connect` ip:port from adb's mDNS list,
/// matching by serial (authoritative - survives IP and port changes) when known,
/// else by IP. Used to recover after the Wireless-debugging port changes
/// (toggling it, or a reboot, assigns a new random port).
fn discover_device_addr(serial: &Option<String>, ip: &str) -> Option<String> {
    // Uses mdns-sd (qr_pairing) rather than `adb mdns services`, which returns
    // nothing in some environments (virtual machines, for one). Match by
    // serial when known.
    qr_pairing::discover_connect_for(serial.as_deref(), ip, Duration::from_secs(5))
}

#[tauri::command]
async fn connect_and_grant(
    app: tauri::AppHandle,
    host_port: String,
    serial: Option<String>,
) -> ConnectResult {
    let host_port = host_port.trim().to_string();
    if host_port.is_empty() {
        return ConnectResult {
            connect: "Enter the IP:Port shown on the phone's Wireless debugging screen.".to_string(),
            grant: String::new(),
            host_port,
            notice: String::new(),
            phone_app: String::new(),
        };
    }

    let adb = adb_path();
    let mut target = host_port.clone();
    let mut connect = run_process(&adb, &["connect", &target]);

    // If the saved port is stale (Wireless debugging was toggled or the phone
    // rebooted -> new random port), re-resolve the phone's current
    // _adb-tls-connect port (by serial, else IP) from mDNS and retry once.
    if !connect_succeeded(&connect) {
        let ip = target.split(':').next().unwrap_or("").to_string();
        if let Some(fresh) = discover_device_addr(&serial, &ip) {
            if fresh != target {
                let retry = run_process(&adb, &["connect", &fresh]);
                if connect_succeeded(&retry) {
                    target = fresh;
                    connect = retry;
                }
            }
        }
    }

    // Identity check for whichever address the block above settled on, run
    // ONCE and before anything is granted or matriculated - also after a plain
    // successful `adb connect` to the saved address: all an attacker needs is
    // DHCP handing the phone's old ip:port to some other machine.
    //
    // An mDNS name is free text anyone on the Wi-Fi can publish with our
    // serial in it (see qr_pairing::discover_connect_for) and a saved ip:port
    // is just an address, so `getprop ro.serialno` over the transport we
    // actually opened is the only identity here that is not chosen by whoever
    // else is on the network. A device that answers nothing (None) is treated
    // as a mismatch.
    //
    // Only when the connection is up: a failed connect leaves no transport
    // for `pm grant` to reach anyway, and reporting a bogus "wrong phone"
    // instead of the real connection error would hide it from the user and
    // from the frontend's retry. A devices.json row with no `serial` carries
    // nothing to compare against, so it goes unverified until its next
    // pairing stamps a serial on it.
    if connect_succeeded(&connect) {
        if let Some(expected) = serial.as_deref() {
            let reported = transport_serial(&target, Duration::from_secs(8));
            if reported.as_deref() != Some(expected) {
                let _ = run_process(&adb, &["disconnect", &target]);
                return ConnectResult {
                    connect: format!(
                        "The device found at {} isn't the saved phone (it reports serial {}, expected {}). Disconnected without granting anything - check that the right phone is on this network.",
                        target,
                        reported.as_deref().unwrap_or("unknown"),
                        expected
                    ),
                    grant: String::new(),
                    // The saved address, not the stranger's: the frontend
                    // persists host_port when it differs.
                    host_port,
                    notice: String::new(),
                    phone_app: String::new(),
                };
            }
        }
    }

    // Is the phone app there? Asked here, after the identity check and before
    // anything is granted: this PC app carries the phone app and installs it
    // with a button, and a `pm grant` for a missing app only gets Android's
    // "Unknown package" back. Missing: nothing is granted or matriculated (both
    // are about the app that is not there), the wizard shows the install button
    // and connects again afterwards. Outdated: everything goes on and the
    // wizard offers the update. Unknown (adb could not say): no claim either way.
    let mut phone_app = String::new();
    if connect_succeeded(&connect) {
        let state = phone_app_state(
            query_installed_phone_app(&adb, &target),
            bundled_phone_app_version(),
        );
        phone_app = state.as_str().to_string();
        if state == PhoneAppState::Missing {
            return ConnectResult {
                connect,
                grant: "Not yet - the Mirryl app is not installed on this phone.".to_string(),
                host_port: target,
                notice: String::new(),
                phone_app,
            };
        }
    }

    let mut grant = run_process(
        &adb,
        &[
            "-s",
            &target,
            "shell",
            "pm",
            "grant",
            PACKAGE_NAME,
            "android.permission.WRITE_SECURE_SETTINGS",
        ],
    );
    if grant.is_empty() {
        grant = "Granted (no output means success).".to_string();
    }
    // Battery whitelist, set here the same way as the permission above.
    // Without it Android files the phone app under "rare" and defers its
    // alarm and jobs, and Wireless debugging may not come back on its own
    // after a Wi-Fi drop. The APK does not ask for this itself (it declares no
    // REQUEST_IGNORE_BATTERY_OPTIMIZATIONS). The watcher repeats it with every
    // seal (watcher.ps1, Set-BatteryExemption). Failing it is not worth
    // failing the connection over: the phone still works, only slower to
    // come back.
    let _ = run_process(
        &adb,
        &["-s", &target, "shell", "dumpsys", "deviceidle", "whitelist", &format!("+{PACKAGE_NAME}")],
    );
    let mut notice = String::new();
    if connect_succeeded(&connect) {
        notice = matriculation_notice(matriculation::matriculate(&app, &adb, &target));
    }
    ConnectResult { connect, grant, host_port: target, notice, phone_app }
}

// ---------- The phone app, shipped inside this PC app ----------
//
// The Mirryl phone app is not published on its own anywhere: no app store,
// no download. The signed APK travels as a bundle resource,
// `phone-app/Mirryl.apk`, next to `phone-app/Mirryl.apk.version`, which holds
// its versionCode. Both are staged by build.rs from the Android build, and
// the Android build script writes the version file next to the APK it signs,
// so the two cannot come from different builds.

const PHONE_APP_DIR: &str = "phone-app";
const PHONE_APP_APK: &str = "Mirryl.apk";
const PHONE_APP_VERSION_FILE: &str = "Mirryl.apk.version";
/// The launcher activity, opened once after installing: Android keeps a
/// freshly installed app in the "stopped" state, which receives no broadcasts
/// (BOOT_COMPLETED, the network ones) until the app has been opened once, and
/// without those the phone app never brings Wireless debugging back by itself.
const PHONE_APP_ACTIVITY: &str = "com.mirryl.app/com.mirryl.app.MainActivity";
/// `adb install` over Wi-Fi of a ~200 KB APK takes seconds, but the phone may
/// scan it first (Play Protect) or wait on a confirmation on its own screen.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// What `dumpsys package com.mirryl.app` says about the phone app. `pm path`
/// is not used: it fails on at least one Samsung phone even with the app installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstalledPhoneApp {
    Missing,
    Version(u32),
    /// adb answered with an error or nothing at all: no claim either way.
    Unknown,
}

fn parse_installed_phone_app(dumpsys: &str, package: &str) -> InstalledPhoneApp {
    let text = dumpsys.trim();
    let marker = format!("Package [{package}]");
    if let Some(at) = text.find(&marker) {
        let section = &text[at..];
        // The first versionCode after the package header is this package's;
        // dumpsys prints it as "versionCode=24 minSdk=21 targetSdk=34".
        return section
            .find("versionCode=")
            .map(|i| &section[i + "versionCode=".len()..])
            .and_then(|rest| {
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse().ok()
            })
            .map(InstalledPhoneApp::Version)
            .unwrap_or(InstalledPhoneApp::Unknown);
    }
    if text.contains("Unable to find package") {
        return InstalledPhoneApp::Missing;
    }
    // What adb or run_process print instead of an answer from the phone.
    let lower = text.to_lowercase();
    if text.is_empty()
        || lower.starts_with("error")
        || lower.starts_with("timed out")
        || lower.contains("device offline")
        || lower.contains("no devices/emulators")
        || lower.contains("device unauthorized")
    {
        return InstalledPhoneApp::Unknown;
    }
    // Older Androids print dumpsys's other sections and simply no package
    // section for a package that is not installed.
    InstalledPhoneApp::Missing
}

fn query_installed_phone_app(adb: &str, target: &str) -> InstalledPhoneApp {
    let out = run_process(adb, &["-s", target, "shell", "dumpsys", "package", PACKAGE_NAME]);
    parse_installed_phone_app(&out, PACKAGE_NAME)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhoneAppState {
    Ready,
    Missing,
    Outdated,
    Unknown,
}

impl PhoneAppState {
    fn as_str(self) -> &'static str {
        match self {
            PhoneAppState::Ready => "ready",
            PhoneAppState::Missing => "missing",
            PhoneAppState::Outdated => "outdated",
            PhoneAppState::Unknown => "unknown",
        }
    }
}

/// Only an installed version strictly older than the bundled one is offered
/// an update; a newer one (a test build on the phone) is left alone, and so is
/// any version when this build carries no phone app to compare with.
fn phone_app_state(installed: InstalledPhoneApp, bundled: Option<u32>) -> PhoneAppState {
    match (installed, bundled) {
        (InstalledPhoneApp::Missing, _) => PhoneAppState::Missing,
        (InstalledPhoneApp::Version(v), Some(b)) if v < b => PhoneAppState::Outdated,
        (InstalledPhoneApp::Version(_), _) => PhoneAppState::Ready,
        (InstalledPhoneApp::Unknown, _) => PhoneAppState::Unknown,
    }
}

fn bundled_phone_app_dir() -> Option<PathBuf> {
    app_dir().map(|dir| dir.join(PHONE_APP_DIR))
}

fn parse_phone_app_version_file(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

/// The versionCode of the APK this build carries, or None when it carries
/// none (an app-only dev build: build.rs warns about it, and `cargo tauri
/// build` refuses to package without it).
fn bundled_phone_app_version() -> Option<u32> {
    let dir = bundled_phone_app_dir()?;
    if !dir.join(PHONE_APP_APK).is_file() {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(PHONE_APP_VERSION_FILE)).ok()?;
    parse_phone_app_version_file(&text)
}

/// The user-facing reading of an `adb install` that did not end in Success.
/// The raw output always follows, for whoever has to look into it.
fn explain_install_failure(output: &str) -> String {
    let reason = if output.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE")
        || output.contains("INSTALL_FAILED_SHARED_USER_INCOMPATIBLE")
    {
        // A copy signed with another key (a build signed with a different
        // keystore). Never uninstalled from here:
        // uninstalling wipes the app's data on the phone, and that is the
        // owner's call, made on the phone.
        "The phone already has a copy of the Mirryl app that was signed differently, and Android won't replace it. Uninstall Mirryl on the phone (Settings > Apps > Mirryl > Uninstall), then press Install again."
    } else if output.contains("INSTALL_FAILED_VERSION_DOWNGRADE") {
        "The phone already has a newer Mirryl app than the one this PC carries. Nothing was changed; it can be used as it is."
    } else if output.contains("INSTALL_FAILED_INSUFFICIENT_STORAGE") {
        "The phone doesn't have enough free space for the Mirryl app. Free some space, then press Install again."
    } else if output.contains("INSTALL_FAILED_USER_RESTRICTED")
        || output.contains("INSTALL_FAILED_ABORTED")
        || output.contains("INSTALL_FAILED_VERIFICATION_FAILURE")
        || output.to_lowercase().contains("user rejected")
    {
        "The phone refused the installation. If it asked for confirmation, accept it. On Samsung phones, Auto Blocker (Settings > Security and privacy) can block installs from a computer: turn it off for a moment, then press Install again."
    } else if output.starts_with("Timed out") {
        "The phone didn't answer in time. Make sure it is awake and unlocked, and look for a confirmation on its screen, then press Install again."
    } else {
        "The Mirryl app could not be installed on the phone."
    };
    format!("{reason}\n\nDetails from Android: {}", output.trim())
}

#[derive(Serialize)]
struct InstallPhoneAppResult {
    ok: bool,
    message: String,
}

/// Installs (or updates) the phone app on a connected phone, from the APK this
/// build carries, then opens it once. Only on the user's press of the button
/// the wizard shows for a phone that lacks the app or has an older one.
#[tauri::command]
async fn install_phone_app(host_port: String) -> InstallPhoneAppResult {
    let fail = |message: String| InstallPhoneAppResult { ok: false, message };
    let target = host_port.trim().to_string();
    if target.is_empty() {
        return fail("Connect to the phone first.".to_string());
    }
    let Some(apk) = bundled_phone_app_dir()
        .map(|dir| dir.join(PHONE_APP_APK))
        .filter(|apk| apk.is_file())
    else {
        return fail(
            "This copy of Mirryl doesn't include the phone app. Reinstall Mirryl on this PC, then try again."
                .to_string(),
        );
    };
    let adb = adb_path();
    let apk_path = apk.to_string_lossy().to_string();
    // -r: replace an older copy in place, keeping its data. A copy signed with
    // another key is refused by Android and explained, never removed.
    let out = run_process_timeout(&adb, &["-s", &target, "install", "-r", &apk_path], INSTALL_TIMEOUT);
    log_activity(&format!("install_phone_app {target}: {}", out.replace('\n', " | ")));
    if !out.lines().any(|line| line.trim() == "Success") {
        return fail(explain_install_failure(&out));
    }
    // Opened once, see PHONE_APP_ACTIVITY. Its result does not decide
    // anything: the app is installed either way.
    let _ = run_process(&adb, &["-s", &target, "shell", "am", "start", "-n", PHONE_APP_ACTIVITY]);
    InstallPhoneAppResult {
        ok: true,
        message: "The Mirryl app is installed on the phone and was opened once, so it can bring Wireless debugging back by itself.".to_string(),
    }
}

/// What the user should read after a matriculation attempt, or "" when
/// nothing. A pc_identity.json that DPAPI cannot decrypt (a Windows password
/// reset by an administrator, a restored profile, a recreated account) makes
/// matriculation stop happening, and a line in activity.log alone would go
/// unnoticed.
fn matriculation_notice(outcome: matriculation::MatriculationOutcome) -> String {
    use matriculation::MatriculationOutcome;
    match outcome {
        // Off Windows there is no DPAPI, so the identity file can never be read
        // and nothing is wrong: phones simply stay on the 30-day seal. Saying
        // "a Windows password reset" at every mirror start would be noise.
        #[cfg(not(windows))]
        MatriculationOutcome::IdentityUnavailable => String::new(),
        #[cfg(windows)]
        MatriculationOutcome::IdentityUnavailable => "This PC's identity file \
             (pc_identity.json in Mirryl's data folder) can't be read - that happens after a \
             Windows password reset by an administrator, a restored profile or a recreated \
             account. Phones stay on the 30-day seal until it is fixed; details are in \
             activity.log."
            .to_string(),
        MatriculationOutcome::SealOnly | MatriculationOutcome::Matriculated => String::new(),
    }
}

#[tauri::command]
async fn restart_adb() -> String {
    let adb = adb_path();
    let kill = run_process(&adb, &["kill-server"]);
    let start = run_process(&adb, &["start-server"]);
    format!(
        "kill-server: {}\nstart-server: {}",
        if kill.is_empty() { "ok".to_string() } else { kill },
        if start.is_empty() { "ok".to_string() } else { start }
    )
}

/// Whether `adb devices` currently lists this exact transport as `device`.
/// Accepts any transport form - `ip:port`, the mDNS name, or a USB serial.
fn adb_device_present(serial: &str) -> bool {
    is_present_in(&run_process(&adb_path(), &["devices"]), serial)
}

/// The pure half of `adb_device_present`, so the tests can drive it.
fn is_present_in(listing: &str, serial: &str) -> bool {
    listing
        .lines()
        .filter_map(parse_adb_device_line)
        .any(|(address, state)| address == serial && state == "device")
}

#[tauri::command]
async fn get_status() -> String {
    run_process(&adb_path(), &["devices", "-l"])
}

/// Discovers the phone's current ADB-over-WiFi IP:port via mDNS, the same
/// way the PC watcher does after every reboot (the port changes randomly).
#[tauri::command]
async fn discover_port() -> String {
    // mDNS discovery only ever finds wireless devices.
    let output = run_process(&adb_path(), &["mdns", "services"]);
    let re = regex::Regex::new(r"\d{1,3}(?:\.\d{1,3}){3}:\d+").expect("valid regex");
    let matches: Vec<String> = re
        .find_iter(&output)
        .map(|m| m.as_str().to_string())
        .collect();
    if matches.is_empty() {
        "No devices found via mDNS yet. Make sure Wireless debugging is on and the phone and this PC are on the same network.".to_string()
    } else {
        matches.join("\n")
    }
}

/// adb often lists the same physical device twice (once by IP:port, once by
/// its mDNS service name), and the engine needs one explicit serial.
/// Prefer the IP:port form since that's what the rest of this app already uses.
fn preferred_device_serial() -> Option<String> {
    pick_preferred_serial(&run_process(&adb_path(), &["devices", "-l"]))
}

/// The pure half of `preferred_device_serial`, so the tests can drive it.
fn pick_preferred_serial(output: &str) -> Option<String> {
    // Not `\S+`: an mDNS transport renamed by a name collision contains a
    // SPACE (`adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp`). Match up to
    // the state keyword instead and take everything before it.
    let device_line = regex::Regex::new(r"(?m)^(.*?)\s+device(?:\s|$)").expect("valid regex");
    let is_ip_port = regex::Regex::new(r"^\d{1,3}(?:\.\d{1,3}){3}:\d+$").expect("valid regex");

    let mut fallback: Option<String> = None;
    for cap in device_line.captures_iter(output) {
        let serial = cap[1].to_string();
        if is_ip_port.is_match(&serial) {
            return Some(serial);
        }
        if fallback.is_none() {
            fallback = Some(serial);
        }
    }
    fallback
}

/// Build a filename-safe fragment out of a device serial.
///
/// A serial is either `ip:port` or the mDNS transport name
/// (`adb-<SERIAL>-xxxx._adb-tls-connect._tcp`). The colon is illegal in a
/// Windows filename and the mDNS form is long, so anything outside a safe set
/// becomes `_` and the result is capped to keep the whole path well clear of
/// MAX_PATH.
fn sanitize_for_filename(value: &str) -> String {
    let mut safe: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    safe.truncate(60);
    // A dot is allowed on purpose, so "Pixel 7.1" keeps its dot. But that lets
    // `.` and `..` through whole, and three callers join this on as an entire
    // path component: `tools_output_dir`, `rename_tools_output_dir` and
    // `delete_tools_folder`. Windows collapses `..` LEXICALLY, before touching
    // the disk, so `Documents\Mirryl\..` IS `Documents` - and it does not need
    // `Mirryl` to exist for that to resolve.
    //
    // Unguarded, a device named `..`, plus the "delete its files" checkbox when
    // removing it, would make `delete_tools_folder` call `remove_dir_all` on the
    // user's entire Documents folder. No dialog, no way back. The frontend does
    // not block it either - `startEditAlias` only checks that the new alias is
    // non-empty and different.
    //
    // Guarded here, once, rather than at each call site: the next caller that
    // joins this onto a directory inherits the fix instead of the bug.
    if safe.is_empty() || safe.chars().all(|c| c == '.') {
        "desconocido".to_string()
    } else {
        safe
    }
}

// ---------- One mirror window per device ----------
//
// Two windows on the same phone show the same picture, but each one is a whole
// engine: its own agent encoding on the phone, its own H.264 decode on the PC,
// so a handful of idle clicks on a connected row must not pile real work on
// both machines for nothing.
//
// The registry is keyed by the DEVICE, not by the address it answers at. One
// phone is one device whether it is reached over USB, over `ip:port` or under
// its mDNS name, and its wireless port changes on its own between sessions -
// keying on the address would allow one window per transport.
//
// Nothing polls to keep it honest: the thread that already waits on the child
// to reap it drops the entry, so closing the mirror window frees the device.
fn open_mirrors() -> &'static Mutex<HashMap<String, u32>> {
    static OPEN: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    OPEN.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Where each open mirror reports the link it is actually using.
///
/// Kept beside the pid map rather than merged into it: the path is derived from
/// the device serial, the registry is keyed by the row's id, and only the launch
/// site knows both. A second map is cheaper than reshaping the first.
fn mirror_transport_files() -> &'static Mutex<HashMap<String, PathBuf>> {
    static FILES: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    FILES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn open_mirror_pid(key: &str) -> Option<u32> {
    open_mirrors().lock().ok()?.get(key).copied()
}

/// Removes a mirror from both registries at once.
///
/// Both, always: a transport file left behind would keep the row announcing
/// the link of an engine that no longer exists.
fn forget_mirror(key: &str) {
    if let Ok(mut open) = open_mirrors().lock() {
        open.remove(key);
    }
    if let Ok(mut files) = mirror_transport_files().lock() {
        files.remove(key);
    }
}

/// Is that process still alive?
///
/// The system is asked about that specific process instead of listing them
/// all: a pid recycled by Windows would give a false "alive" with any method,
/// but by waiting on the handle opened here the window is milliseconds, not
/// minutes.
#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    type Handle = *mut std::ffi::c_void;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const WAIT_TIMEOUT: u32 = 0x0000_0102;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    unsafe {
        let handle = OpenProcess(SYNCHRONIZE, 0, pid);
        if handle.is_null() {
            // It cannot even be opened: as far as we care, it is not there.
            return false;
        }
        // No wait: only asking whether it has already finished.
        let state = WaitForSingleObject(handle, 0);
        CloseHandle(handle);
        state == WAIT_TIMEOUT
    }
}

/// Linux: `/proc/<pid>/stat` reads "pid (name) S ..."; a process that has
/// exited but not been reaped shows as Z, and that is not alive for our
/// purposes.
#[cfg(not(windows))]
fn process_is_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z"),
        Err(_) => false,
    }
}

#[derive(Serialize)]
struct OpenMirror {
    /// The device id whose mirror is open.
    key: String,
    /// The link that mirror is on right now - "usb", "wifi", or "" when the
    /// engine has not reported yet. NOT the device's best available link: while
    /// a mirror is open those two differ the moment a cable is plugged in, and
    /// showing the second would make the row claim a USB mirror that does not
    /// exist.
    kind: String,
}

/// Which devices this app has a mirror open for, and over which link.
#[tauri::command]
fn list_open_mirrors() -> Vec<OpenMirror> {
    let keys: Vec<String> = match open_mirrors().lock() {
        Ok(open) => open.keys().cloned().collect(),
        Err(_) => return Vec::new(),
    };
    let files = mirror_transport_files().lock().ok();
    keys.into_iter()
        .map(|key| {
            let kind = files
                .as_ref()
                .and_then(|files| files.get(&key))
                .and_then(|path| std::fs::read_to_string(path).ok())
                .map(|text| text.trim().to_string())
                .filter(|text| text == "usb" || text == "wifi")
                .unwrap_or_default();
            OpenMirror { key, kind }
        })
        .collect()
}

/// Bring an already-open mirror to the front. Answers:
///   "focused"   - its window is now in front.
///   "gone"      - nothing is open for that device; it was closed since the
///                 caller last looked, so the caller should just open it.
///   "not-found" - the engine is alive but none of its windows could be found.
#[tauri::command]
fn focus_mirror(key: String) -> String {
    let Some(pid) = open_mirror_pid(&key) else {
        return "gone".to_string();
    };
    // An engine that DIED leaves its key in place with a pid that no longer
    // exists. Without this check no window is found for it and this returns
    // "not-found", which the frontend turns into "already open" and stops
    // there: the row would be useless until the app restarted.
    //
    // "gone" is the path that ALREADY exists and that reopens the mirror, so
    // reaching it is enough.
    if !process_is_alive(pid) {
        forget_mirror(&key);
        return "gone".to_string();
    }
    if focus_process_window(pid) {
        "focused".to_string()
    } else {
        "not-found".to_string()
    }
}

/// Raise the window that belongs to `pid`.
///
/// A handful of user32 imports by hand: windows-sys is built here without its
/// windowing features, and these few calls are not worth switching them on.
#[cfg(windows)]
fn focus_process_window(pid: u32) -> bool {
    use std::os::raw::c_int;

    type Hwnd = *mut std::ffi::c_void;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(
            callback: unsafe extern "system" fn(Hwnd, isize) -> i32,
            param: isize,
        ) -> i32;
        fn GetWindowThreadProcessId(window: Hwnd, pid: *mut u32) -> u32;
        fn IsWindowVisible(window: Hwnd) -> i32;
        fn IsIconic(window: Hwnd) -> i32;
        fn GetWindowTextW(window: Hwnd, buffer: *mut u16, len: c_int) -> c_int;
        fn ShowWindow(window: Hwnd, command: c_int) -> i32;
        fn SetForegroundWindow(window: Hwnd) -> i32;
        fn GetForegroundWindow() -> Hwnd;
        fn BringWindowToTop(window: Hwnd) -> i32;
        fn AttachThreadInput(attaching: u32, attached_to: u32, attach: i32) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThreadId() -> u32;
    }

    const SW_RESTORE: c_int = 9;

    /// Put `window` in front, including when Windows would rather we didn't.
    ///
    /// SetForegroundWindow alone is enough in the case that matters - Mirryl is
    /// the foreground process at the instant the user clicks one of its rows,
    /// and Windows grants the foreground to whoever already holds it. But it is
    /// refused outright from any other situation, and then the call quietly
    /// returns false and only flashes the taskbar button. Attaching our input
    /// queue to the current foreground window's thread puts us inside its input
    /// context for a moment, which is the sanctioned way through that rule.
    /// Measured: from a plain console process with Chrome in front, the plain
    /// call fails and this path succeeds.
    unsafe fn raise(window: Hwnd) -> bool {
        if IsIconic(window) != 0 {
            ShowWindow(window, SW_RESTORE);
        }
        if SetForegroundWindow(window) != 0 {
            return true;
        }
        let foreground = GetForegroundWindow();
        if foreground.is_null() {
            return false;
        }
        let mut ignored = 0u32;
        let current = GetCurrentThreadId();
        let foreground_thread = GetWindowThreadProcessId(foreground, &mut ignored);
        let target_thread = GetWindowThreadProcessId(window, &mut ignored);
        // Attaching a thread to itself is an error, not a no-op.
        let attached_foreground =
            foreground_thread != current && AttachThreadInput(current, foreground_thread, 1) != 0;
        let attached_target = target_thread != current
            && target_thread != foreground_thread
            && AttachThreadInput(current, target_thread, 1) != 0;
        BringWindowToTop(window);
        let ok = SetForegroundWindow(window) != 0;
        if attached_target {
            AttachThreadInput(current, target_thread, 0);
        }
        if attached_foreground {
            AttachThreadInput(current, foreground_thread, 0);
        }
        // Trust what is actually on screen over the return value: the call can
        // report success having done no more than flash the taskbar button.
        ok || GetForegroundWindow() == window
    }

    struct Search {
        pid: u32,
        /// The engine's own window, recognised by its title.
        titled: Hwnd,
        /// Any other visible window of that process, in case the engine's title
        /// ever changes - better than refusing to focus anything.
        fallback: Hwnd,
    }

    unsafe extern "system" fn visit(window: Hwnd, param: isize) -> i32 {
        let search = &mut *(param as *mut Search);
        let mut owner = 0u32;
        GetWindowThreadProcessId(window, &mut owner);
        if owner != search.pid || IsWindowVisible(window) == 0 {
            return 1; // not ours: keep enumerating
        }
        let mut buffer = [0u16; 128];
        let len = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as c_int).max(0);
        let title = String::from_utf16_lossy(&buffer[..len as usize]);
        // The engine owns a few helper windows besides the mirror (WebView2 puts
        // up its own); the titled one is the mirror. See viewer.rs, which builds
        // it as "Mirryl Connect — <model>".
        if title.starts_with("Mirryl Connect") {
            search.titled = window;
            return 0; // found it: stop enumerating
        }
        if search.fallback.is_null() && !title.is_empty() {
            search.fallback = window;
        }
        1
    }

    let mut search = Search {
        pid,
        titled: std::ptr::null_mut(),
        fallback: std::ptr::null_mut(),
    };
    unsafe { EnumWindows(visit, &mut search as *mut Search as isize) };
    let window = if search.titled.is_null() {
        search.fallback
    } else {
        search.titled
    };
    if window.is_null() {
        return false;
    }
    unsafe { raise(window) }
}

#[cfg(not(windows))]
fn focus_process_window(_pid: u32) -> bool {
    false
}

// ---------- Mirrors in tabs ----------
//
// Each mirror is its own engine process - one phone freezing cannot take the
// others or the app down - and it opens with no window: the engine serves its
// page on 127.0.0.1 and the app shows that page in a tab. The two
// talk over the engine's stdin and stdout, one line each way:
//
//   app -> engine   "window"  take the mirror out to a window of its own
//                   "tab"     close that window, keep the session
//                   "close"   end the session
//   engine -> app   "MIRRYL-READY <url>"          the page is up
//                   "MIRRYL-WINDOW"               it is in its own window now
//                   "MIRRYL-TAB"                  it is back, show it again
//                   "MIRRYL-WINDOW-FAILED <why>"  the window could not open
//                   "MIRRYL-CLIPBOARD <hex>"      Linux only: the phone's copied
//                                                 text, for the PC clipboard
//
// Everything else the engine prints goes to its log file. The clipboard line
// does not: what the user copied is not the log's business.

/// The stdin of every mirror running in this app, by device key.
fn mirror_inputs() -> &'static Mutex<HashMap<String, std::process::ChildStdin>> {
    static INPUTS: OnceLock<Mutex<HashMap<String, std::process::ChildStdin>>> = OnceLock::new();
    INPUTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What the page hears about a mirror, as the `mirror-event` event.
#[derive(Clone, Serialize)]
struct MirrorEvent {
    key: String,
    /// "ready", "window", "tab", "window-failed" or "exit".
    kind: String,
    /// The page's address for "ready"; the reason for "window-failed"; for
    /// "exit", the engine's last error line, empty when it simply closed.
    detail: String,
}

fn emit_mirror_event(app: &tauri::AppHandle, key: &str, kind: &str, detail: &str) {
    use tauri::Emitter;
    let _ = app.emit(
        "mirror-event",
        MirrorEvent {
            key: key.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
        },
    );
}

/// Linux: writes the phone's copied text to the PC clipboard, for a mirror
/// that sits in one of this app's tabs. The engine has no window then, and on
/// Wayland only the client with the keyboard focus may set the clipboard: in a
/// tab, that is this app. GTK's clipboard belongs to the main thread.
#[cfg(target_os = "linux")]
fn set_clipboard_from_engine(app: &tauri::AppHandle, hex_text: &str) {
    let Ok(bytes) = hex::decode(hex_text.trim()) else {
        log_activity("mirror: the engine sent a clipboard line that is not hex");
        return;
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let _ = app.run_on_main_thread(move || {
        gtk::Clipboard::get(&gtk::gdk::SELECTION_CLIPBOARD).set_text(&text);
    });
}

/// Starts an engine that lives in a tab, and follows it until it ends.
fn spawn_in_tab(app: tauri::AppHandle, mut cmd: Command, key: String, log_name: &str) -> String {
    use std::io::{BufRead, Write};
    use std::sync::Arc;

    cmd.env("MIRRYL_EMBEDDED", "1");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return format!("Error: {}", e),
    };
    let pid = child.id();
    if let Ok(mut open) = open_mirrors().lock() {
        open.insert(key.clone(), pid);
    }
    if let (Some(stdin), Ok(mut inputs)) = (child.stdin.take(), mirror_inputs().lock()) {
        inputs.insert(key.clone(), stdin);
    }
    // The engine's log file, overwritten per launch.
    let log = local_data_dir()
        .and_then(|dir| std::fs::File::create(dir.join(log_name)).ok())
        .map(|file| Arc::new(Mutex::new(file)));
    let last_error = Arc::new(Mutex::new(String::new()));

    let out_reader = child.stdout.take().map(|stdout| {
        let (app, key, log) = (app.clone(), key.clone(), log.clone());
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                #[cfg(target_os = "linux")]
                if let Some(hex_text) = line.strip_prefix("MIRRYL-CLIPBOARD ") {
                    set_clipboard_from_engine(&app, hex_text);
                    continue;
                }
                if let Some(log) = &log {
                    if let Ok(mut file) = log.lock() {
                        let _ = writeln!(file, "{line}");
                    }
                }
                if let Some(url) = line.strip_prefix("MIRRYL-READY ") {
                    emit_mirror_event(&app, &key, "ready", url.trim());
                } else if let Some(why) = line.strip_prefix("MIRRYL-WINDOW-FAILED ") {
                    emit_mirror_event(&app, &key, "window-failed", why.trim());
                } else if line == "MIRRYL-WINDOW" {
                    emit_mirror_event(&app, &key, "window", "");
                } else if line == "MIRRYL-TAB" {
                    emit_mirror_event(&app, &key, "tab", "");
                }
            }
        })
    });
    let err_reader = child.stderr.take().map(|stderr| {
        let (log, last_error) = (log.clone(), last_error.clone());
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                if let Some(log) = &log {
                    if let Ok(mut file) = log.lock() {
                        let _ = writeln!(file, "{line}");
                    }
                }
                // The engine ends with "Error: <why>" when it cannot go on;
                // that is the line the tab shows instead of a blank page.
                if let Some(why) = line.strip_prefix("Error: ") {
                    if let Ok(mut last) = last_error.lock() {
                        *last = why.trim().to_string();
                    }
                }
            }
        })
    });

    std::thread::spawn(move || {
        let _ = child.wait();
        // Read to the end first, so the reason is in before the tab hears.
        if let Some(reader) = out_reader {
            let _ = reader.join();
        }
        if let Some(reader) = err_reader {
            let _ = reader.join();
        }
        let still_ours = open_mirrors()
            .lock()
            .map(|open| open.get(&key) == Some(&pid))
            .unwrap_or(false);
        // Only if it is still OUR engine: the device may have been opened
        // again in the time it took this wait() to return.
        if still_ours {
            forget_mirror(&key);
            if let Ok(mut inputs) = mirror_inputs().lock() {
                inputs.remove(&key);
            }
            let reason = last_error.lock().map(|r| r.clone()).unwrap_or_default();
            emit_mirror_event(&app, &key, "exit", &reason);
        }
    });
    "Opening the mirror...".to_string()
}

/// Tells a mirror to move to its own window ("window"), come back to its tab
/// ("tab") or end ("close"). Answers "ok", or "gone" when nothing is open for
/// that device any more.
#[tauri::command]
fn mirror_command(key: String, command: String) -> String {
    use std::io::Write;
    if !matches!(command.as_str(), "window" | "tab" | "close") {
        return "Error: unknown mirror command".to_string();
    }
    let Ok(mut inputs) = mirror_inputs().lock() else {
        return "gone".to_string();
    };
    let Some(stdin) = inputs.get_mut(&key) else {
        return "gone".to_string();
    };
    match writeln!(stdin, "{command}").and_then(|_| stdin.flush()) {
        Ok(()) => "ok".to_string(),
        Err(_) => {
            inputs.remove(&key);
            "gone".to_string()
        }
    }
}

#[tauri::command]
async fn start_mirror(
    app: tauri::AppHandle,
    host_port: Option<String>,
    device_key: Option<String>,
    wifi_address: Option<String>,
    alias: Option<String>,
) -> String {
    // Use the caller's specific device when given (Device Manager knows which
    // row was clicked); otherwise auto-detect the one connected device.
    let serial = host_port.or_else(preferred_device_serial);
    // Refresh the phone's stamp on every mirror session, cable or wireless -
    // see matriculation::matriculate for why this exists. Only for a phone
    // that is in the list: a cable phone mirrored from the offer row has no
    // row, so there is nothing for it to reconnect to on its own.
    if let Some(target) = serial.as_deref() {
        if is_listed(&read_devices(&app), target) {
            // A cable phone never goes through connect_and_grant, so this is
            // the only place its identity notice can come from: an event the
            // page turns into the same dialog.
            let notice = matriculation_notice(matriculation::matriculate(&app, &adb_path(), target));
            if !notice.is_empty() {
                use tauri::Emitter;
                let _ = app.emit("mirryl-notice", notice);
            }
        }
    }
    // The registry key is the row's device id whenever the caller knows it.
    // Falling back to the transport is better than nothing, but it is the weaker
    // key: the same phone has a different one on USB than on Wi-Fi.
    let key = device_key.or_else(|| serial.clone());
    start_mirror_engine(app, serial, key, wifi_address, alias)
}

/// Mirror with Mirryl's own engine, on Windows and on Linux.
///
/// The engine is a separate process per device by design: it owns its own
/// page (and window, when it leaves its tab), Android agent, loopback port and
/// session token, so one phone freezing or disconnecting cannot take the others
/// (or this launcher) down.
fn start_mirror_engine(
    app: tauri::AppHandle,
    serial: Option<String>,
    key: Option<String>,
    wifi_address: Option<String>,
    alias: Option<String>,
) -> String {
    let Some(engine) = engine_program() else {
        return format!(
            "The Mirryl mirroring engine (engine{}{}) was not found next to this app. Something went wrong with the app's packaging.",
            std::path::MAIN_SEPARATOR,
            ENGINE_BUNDLED_NAME
        );
    };
    // The engine has no "just pick the only device" mode - it always mirrors
    // an explicit serial, so say so instead of failing silently.
    let Some(serial) = serial else {
        return "No connected device to mirror yet. Connect the phone first.".to_string();
    };
    let key = key.unwrap_or_else(|| serial.clone());

    // Already mirroring this phone: raise that window instead of starting a
    // second engine. The Device Manager normally catches this before it gets
    // here (its row knows), but this is the check that actually holds - it is
    // the one place that knows whether the process is still alive.
    if let Some(pid) = open_mirror_pid(&key) {
        return if focus_process_window(pid) {
            "Already open - brought its window to the front.".to_string()
        } else {
            "Already open - look for its \"Mirryl Connect\" window on the taskbar.".to_string()
        };
    }

    // Check the device is really there BEFORE launching the engine. The engine
    // is a separate process, and its own error for a phone it cannot reach
    // looks like the app broke rather than like a phone that is simply not
    // connected. Failing here keeps the explanation inside Mirryl.
    if !adb_device_present(&serial) {
        return format!(
            "{serial} is not connected right now. Check that the phone is on the same \
Wi-Fi network with Wireless debugging on, or plug it in by USB."
        );
    }

    let mut cmd = Command::new(&engine);
    cmd.arg("mirror").arg(&serial);
    // Belt and braces: if the engine fails for a reason we could not predict,
    // it writes to its log instead of putting a native dialog on screen.
    cmd.env("MIRADOR_NO_DIALOG", "1");
    // CRITICAL: point the engine at the exact same adb binary this app uses.
    // Its own lookup order would otherwise be able to find a different adb on
    // PATH, and two different adb builds each start their own server ("adb
    // server version doesn't match") - which kills the connection this app
    // just made. One app, one adb, one server.
    cmd.env("MIRADOR_ADB", adb_path());
    cmd.env("MIRRYL_DETACHED", "1");
    // When the phone drops off mid-session, the engine's window offers to move
    // the mirror to the other kind of link. It can find a USB phone on its own
    // (the transport IS the hardware serial), but not a wireless one it has
    // never seen: only this app knows which phones are paired for wireless
    // debugging, and telling "not paired" apart from "paired but not on the
    // network" is the difference between a useless error and an actionable one.
    if let Some(address) = wifi_address.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        cmd.env("MIRRYL_WIFI_ADDRESS", address);
    }
    // The engine only ever sees a bare serial/address - devices.json (and the
    // alias in it) is this app's own. Without handing it over, the window
    // title and diagnostics log fall back to the phone's raw model name no
    // matter what the user renamed the row to in Device Manager.
    if let Some(name) = alias.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
        cmd.env("MIRRYL_DEVICE_ALIAS", name);
    }
    // The engine writes the link it is really on here, and the device list reads
    // it back. Without this the row shows the phone's BEST available link, so
    // plugging a cable into a Wi-Fi session would flip the icon to USB while
    // the mirror is still on Wi-Fi.
    if let Some(dir) = local_data_dir() {
        // The engine's per-session diagnostics (`.jsonl`) go here too. Same
        // rule as the files below - the app decides where the engine's state
        // lives.
        cmd.env("MIRRYL_LOG_DIR", dir.join("engine").join("logs"));
        let path = dir.join(format!(
            "mirryl-engine-{}.transport",
            sanitize_for_filename(&serial)
        ));
        // Clear any value left by an earlier session, so "not reported yet"
        // reads as unknown instead of as last time's answer.
        let _ = std::fs::remove_file(&path);
        cmd.env("MIRRYL_TRANSPORT_FILE", &path);
        if let Ok(mut files) = mirror_transport_files().lock() {
            files.insert(key.clone(), path);
        }
    }
    // Where the mirror window remembers the place it was left. ONE file for all
    // devices - the last window to close wins - which is what keeps this from
    // becoming per-device bookkeeping.
    //
    // In the app-data dir, not next to the exe, so the position survives an
    // update or a move of the program folder.
    if let Some(dir) = roaming_app_dir() {
        let _ = std::fs::create_dir_all(&dir);
        cmd.env("MIRRYL_WINDOW_STATE_FILE", dir.join("window-position.txt"));
    }
    // The user's keyboard shortcuts, read by the engine when it serves its
    // window. Passed even when the file does not exist yet: the engine falls
    // back to its own defaults, and this way the first save starts working
    // without the app having to have created anything up front.
    //
    // Read at window OPEN, so changing a shortcut applies to mirror windows
    // opened afterwards. The settings screen says so rather than pretending
    // otherwise.
    if let Some(path) = keybindings_path_for_engine() {
        cmd.env("MIRRYL_KEYBINDINGS_FILE", path);
    }
    // Same idea, opposite direction: the engine WRITES to this one (a click on
    // "Don't show again" in the mirror window), the app never does. Handed
    // over so the engine still does not get to decide where its own state
    // lives - see window_state_path in native_window.rs for the same rule.
    if let Some(path) = game_hint_path_for_engine() {
        cmd.env("MIRRYL_GAME_HINT_FILE", path);
    }
    // Without this the engine's own hidden adb children still flash console
    // windows on screen.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    spawn_in_tab(
        app,
        cmd,
        key,
        &format!("mirryl-engine-{}.log", sanitize_for_filename(&serial)),
    )
}

// ---------- File transfer ----------
// Plain `adb push` to the phone's Download folder.

// `async`, and the wait on its own blocking thread: a plain `fn` command runs
// on the main thread, and the whole window would hang for as long as the
// picker is open.
#[cfg(windows)]
#[tauri::command]
async fn pick_file() -> String {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.OpenFileDialog
$dialog.Title = "Choose a file to send to the phone"
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
    Write-Output $dialog.FileName
}
"#;
    // The page treats anything non-empty as the chosen file, so only a path
    // that exists may leave here - never the runner's own error text, which
    // `adb push` would take for the local path.
    // Cancel is "" and stays ""; anything else is logged and reads as cancel.
    let picked = tauri::async_runtime::spawn_blocking(move || run_powershell_sta(script))
        .await
        .unwrap_or_default();
    let picked = picked.trim();
    if picked.is_empty() || Path::new(picked).is_file() {
        return picked.to_string();
    }
    log_activity(&format!("pick_file: the picker did not return a file: {picked}"));
    String::new()
}

/// Linux: zenity's file chooser (a dependency of the package), with the same
/// contract as the Windows picker - a path that exists, or "" for cancel.
#[cfg(not(windows))]
#[tauri::command]
async fn pick_file() -> String {
    let picked = tauri::async_runtime::spawn_blocking(|| {
        Command::new("zenity")
            .args(["--file-selection", "--title=Choose a file to send to the phone"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    if picked.is_empty() || Path::new(&picked).is_file() {
        return picked;
    }
    log_activity(&format!("pick_file: the picker did not return a file: {picked}"));
    String::new()
}

/// Percent-encodes one path segment for the `file://` URI that push_file
/// hands to `am broadcast`. Everything outside `A-Z a-z 0-9 - _ .` is
/// escaped, which covers two things at once: the URI itself (a space, `#`,
/// `?` or `%` in a file name would end or corrupt it) and the phone's shell -
/// `adb shell` joins its arguments into ONE command line for the device's sh,
/// so an unescaped `My App (1).apk` would be split into words there and the
/// parentheses would be a syntax error. `~` is escaped too, though a URI does
/// not need it, so the shell never gets a look at anything. Android decodes
/// it all back (Uri.getPath), so the scanner sees the real name.
fn percent_encode_uri_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len() * 3);
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => encoded.push(byte as char),
            _ => encoded.push_str(&format!("%{:02X}", byte)),
        }
    }
    encoded
}

#[tauri::command]
async fn push_file(host_port: String, local_path: String) -> String {
    let adb = adb_path();
    let remote_dir = "/sdcard/Download/";
    // A slow transfer is not a hung process: 40 s (PROCESS_TIMEOUT) is a
    // couple of hundred megabytes over Wi-Fi, and killing adb halfway leaves a
    // truncated file on the phone that then gets reported as sent.
    let result = run_process_timeout(
        &adb,
        &["-s", &host_port, "push", &local_path, remote_dir],
        Duration::from_secs(600),
    );
    // Positive check: adb ends a good push with "N file(s) pushed". Looking
    // for "error:" instead would let a timeout ("Timed out after ...") and a
    // failed spawn ("Error running adb") both pass as success.
    if !(result.contains("file pushed") || result.contains("files pushed")) {
        return format!("Error: {}", result);
    }
    // The file's real home: /sdcard is a symlink to /storage/emulated/0, and
    // this spelling is the one the media scanner below and the phone's Files
    // app both use - so it is also the path worth telling the user.
    let file_name = Path::new(&local_path)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let remote_file = format!("/storage/emulated/0/Download/{}", file_name);
    // `adb push` writes the bytes but never tells Android's media store about
    // them, and the phone's Files app lists from that index, not from the
    // folder: without it the file is "Sent" and then nowhere to be found.
    // This broadcast asks the scanner to index just that one file. Deprecated,
    // but it still works on Android 11, 14 and 16.
    // If it fails the send is still a success - the file IS on the phone - so
    // it only ever adds a line to the message, never turns it into an Error.
    let uri = format!(
        "file:///storage/emulated/0/Download/{}",
        percent_encode_uri_segment(&file_name)
    );
    let scan = run_process(
        &adb,
        &[
            "-s",
            &host_port,
            "shell",
            "am",
            "broadcast",
            "-a",
            "android.intent.action.MEDIA_SCANNER_SCAN_FILE",
            "-d",
            &uri,
        ],
    );
    // Only the path goes to the user: adb's own line repeats the local
    // Windows path and transfer statistics, and the dialog that shows this
    // collapses newlines, so it would bury the one thing worth reading.
    let mut message = format!("Sent to {} on the phone.", remote_file);
    if !scan.contains("Broadcast completed") {
        message.push_str(
            " The phone did not confirm indexing it, so its Files app may not list it until the phone restarts - the file itself is there.",
        );
    }
    message
}

// ---------- The two things that need administrator ----------
// Everything else in this app runs as a normal user. These two do not, so they
// are the only callers of run_powershell_elevated, and each one costs the user
// a UAC prompt - which is why `repair_system` does both in a single elevated
// run rather than prompting twice.

/// Rebuilds every firewall rule Mirryl depends on, for this exe path and for
/// the bundled adb.
///
/// It deletes first on purpose. Rules are keyed by program PATH, and Windows
/// silently creates a **Block** rule whenever someone dismisses its "allow this
/// app?" prompt - and a Block rule beats an Allow rule, so adding an Allow on
/// top of one leaves the phone unable to reach the PC in a way that is very
/// hard to guess from the symptom.
#[cfg(windows)]
fn firewall_repair_script() -> String {
    let exe = env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let adb = adb_path();
    format!(
        r#"
$exe = "{exe}"
$adb = "{adb}"
$removed = 0
$orphans = 0
# The five EXACT names the block below creates. The sweep only touches
# these: they are the only ones this app writes, so deleting them cannot
# step on another program's rule. If the New-NetFirewallRule names ever
# change, they must be added here or their old rules will stop being
# cleaned up.
$ourNames = @(
    "Mirryl (inbound)",
    "Mirryl (outbound)",
    "Mirryl ADB (inbound)",
    "Mirryl ADB (outbound)",
    "Mirryl ADB mDNS (UDP 5353)"
)
try {{
    if ($adb -eq "adb") {{
        $found = Get-Command adb -ErrorAction SilentlyContinue
        if ($found) {{ $adb = $found.Source }}
    }}
    foreach ($program in @($exe, $adb)) {{
        if (-not $program) {{ continue }}
        $filters = Get-NetFirewallApplicationFilter -Program $program -ErrorAction SilentlyContinue
        foreach ($f in $filters) {{
            foreach ($rule in ($f | Get-NetFirewallRule -ErrorAction SilentlyContinue)) {{
                Remove-NetFirewallRule -Name $rule.Name -ErrorAction SilentlyContinue
                $removed++
            }}
        }}
    }}
    # Rules are stored by the program's PATH, so the loop above only sees
    # those of the CURRENT path. Every copy run from another folder, and every
    # rename of the executable, leaves its own behind forever.
    #
    # Start FROM the name, not from the filters: walking every filter in the
    # system and checking the name afterwards can take minutes, against the
    # 180 s ceiling of the elevated step. The reason is that many of Windows'
    # own rules store the path with
    # unexpanded variables (%SystemRoot%\...), which Test-Path -LiteralPath
    # treats as missing, so one rule query was paid for each of hundreds.
    # Asking by DisplayName first, a single call returns only ours and the
    # rest of the system is not touched.
    #
    # Two conditions, both mandatory: the name must be one of ours AND the
    # program must be gone from disk. One of our rules pointing at an .exe
    # that DOES exist is left alone - it may be another copy of Mirryl the
    # user wants to keep. (Test-Path -LiteralPath is correct here precisely
    # because in OUR rules the program is always a literal absolute path,
    # never one with variables.)
    foreach ($rule in (Get-NetFirewallRule -DisplayName $ourNames -ErrorAction SilentlyContinue)) {{
        $filter = $rule | Get-NetFirewallApplicationFilter -ErrorAction SilentlyContinue
        $program = $filter.Program
        if (-not $program -or $program -eq "Any") {{ continue }}
        if (Test-Path -LiteralPath $program) {{ continue }}
        Remove-NetFirewallRule -Name $rule.Name -ErrorAction SilentlyContinue
        $orphans++
    }}
    New-NetFirewallRule -DisplayName "Mirryl (inbound)" -Direction Inbound -Program $exe -Action Allow -Profile Any -ErrorAction Stop | Out-Null
    New-NetFirewallRule -DisplayName "Mirryl (outbound)" -Direction Outbound -Program $exe -Action Allow -Profile Any -ErrorAction Stop | Out-Null
    if (Test-Path -LiteralPath $adb) {{
        New-NetFirewallRule -DisplayName "Mirryl ADB (inbound)" -Direction Inbound -Program $adb -Action Allow -Profile Any -ErrorAction Stop | Out-Null
        New-NetFirewallRule -DisplayName "Mirryl ADB (outbound)" -Direction Outbound -Program $adb -Action Allow -Profile Any -ErrorAction Stop | Out-Null
        New-NetFirewallRule -DisplayName "Mirryl ADB mDNS (UDP 5353)" -Direction Inbound -Protocol UDP -LocalPort 5353 -Program $adb -Action Allow -Profile Any -ErrorAction Stop | Out-Null
    }}
    # This sentence is shown as is in the window when Repair succeeds, so it
    # is worded for a person to read. `$removed` is left out on purpose - those
    # are the rules of the current path that get deleted to be created again,
    # i.e. routine, not news; the number that does matter is the one of the
    # rules other copies left behind.
    $msg = "Firewall rules rebuilt."
    if ($orphans -gt 0) {{
        $msg = $msg + " Also cleared out " + $orphans + " rule(s) that earlier installs of Mirryl had left behind."
    }}
    Write-Output $msg
}} catch {{
    Write-Output ("FAILED firewall: " + $_.Exception.Message)
}}
"#,
        exe = exe,
        adb = adb
    )
}

/// Registers (or re-registers) the auto-reconnect scheduled task.
///
/// `-Force` makes this the repair path too: a task pointing at a watcher.ps1
/// that no longer exists - the usual outcome of moving or deleting the folder a
/// previous version lived in - is overwritten with the current path. The same
/// re-registration covers a task pointing at another copy's watcher.ps1 and
/// one left with RunLevel Highest (see `SystemHealth::watcher`).
///
/// `$env:USERNAME` is correct here: UAC elevation keeps the same user account,
/// it just hands it a full token. (It would NOT be correct from an installer's
/// custom action, which runs as SYSTEM.)
///
/// `-RunLevel Limited` is deliberate and must stay that way. Registering an S4U
/// task needs admin, but the task itself does not: watcher.ps1 only runs the
/// bundled adb, appends to watcher.log beside itself, reads and updates
/// devices.json under %APPDATA%, and writes a temp file - every one of those
/// works with a filtered token. `Highest` would be a privilege-escalation hole:
/// the task runs powershell.exe -ExecutionPolicy Bypass over a .ps1 that lives
/// in the install directory, and this installer is currentUser mode, so INSTDIR
/// is %LOCALAPPDATA%\Mirryl - a path the user has FullControl over WITHOUT
/// elevating. Any non-elevated process running as that user could rewrite the
/// script and get a full admin token on the next boot, with no UAC prompt.
#[cfg(windows)]
fn watcher_repair_script(watcher: &str) -> String {
    // `$env:USERNAME` is correct on this path, and only on this one: elevation
    // from the app keeps the account, it only gives it a full token.
    watcher_repair_script_for(watcher, "$env:USERNAME")
}

/// Same as the previous one, but with the user given from outside.
///
/// `user_expr` is a PowerShell EXPRESSION, not a name: either `$env:USERNAME`
/// or an already-quoted literal. It exists because from the installer the
/// assumption above is FALSE - a standard user types another administrator's
/// credentials at the UAC, and the task would be registered for that other
/// person: a task that never runs and nobody knows why.
#[cfg(windows)]
fn watcher_repair_script_for(watcher: &str, user_expr: &str) -> String {
    format!(
        r#"
try {{
    $action = New-ScheduledTaskAction -Execute "powershell.exe" -Argument "-NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File `"{watcher}`""
    $trigger = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId {user_expr} -LogonType S4U -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero)
    Register-ScheduledTask -TaskName "{task}" -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
    Enable-ScheduledTask -TaskName "{task}" -ErrorAction SilentlyContinue | Out-Null
    # The only trigger is AtStartup, so without this the watcher would sit idle
    # until the next reboot - installing it and it not running is exactly the
    # state we are trying to get out of.
    Start-ScheduledTask -TaskName "{task}" -ErrorAction SilentlyContinue
    Write-Output "Auto-reconnect watcher installed for '$env:USERNAME' and started. It runs on every PC startup, with no login needed."
}} catch {{
    Write-Output ("FAILED watcher: " + $_.Exception.Message)
}}
"#,
        watcher = watcher,
        task = WATCHER_TASK_NAME,
        user_expr = user_expr
    )
}

#[cfg(windows)]
#[tauri::command]
async fn firewall_fix() -> String {
    if is_store_package() {
        return STORE_FIREWALL_NOTE.to_string();
    }
    let out = run_powershell_elevated(&firewall_repair_script());
    if is_elevated_early_return(&out) {
        return out;
    }
    if out.contains("FAILED") {
        return format!("Error: {}", out);
    }
    format!("Done. {} Try pairing/connecting again.", out)
}

#[cfg(not(windows))]
#[tauri::command]
async fn firewall_fix() -> String {
    "Automatic firewall configuration isn't available on this platform yet. If pairing or connecting fails, make sure inbound TCP (for ADB) and UDP port 5353 (for mDNS discovery) are allowed through your firewall.".to_string()
}

// ---------- Does this machine still have what Mirryl set up? ----------
// Both checks below are READ-ONLY and work fine without administrator (verified
// from a standard user shell: Get-ScheduledTask and Get-NetFirewallApplication-
// Filter both answer). That is the whole reason this design works: the app can
// notice something is broken for free, and only spend a UAC prompt on fixing it.
//
// It remembers whether the watcher was ever wanted, because "you never installed
// it" and "it was installed and something removed it" deserve different
// treatment - only the second one is worth interrupting anyone about.

#[derive(Serialize, Deserialize, Default)]
#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (watcher, wireless reconnect).
struct WatcherPref {
    installed: bool,
}

#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (watcher, wireless reconnect).
fn watcher_pref_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("watcher.json"))
}

#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (watcher, wireless reconnect).
fn watcher_wanted(app: &tauri::AppHandle) -> bool {
    watcher_pref_path(app)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<WatcherPref>(&s).ok())
        .map(|p| p.installed)
        .unwrap_or(false)
}

#[cfg_attr(not(windows), allow(dead_code))] // Windows-only (watcher, wireless reconnect).
fn set_watcher_wanted(app: &tauri::AppHandle, installed: bool) {
    if let Ok(path) = watcher_pref_path(app) {
        if let Ok(json) = serde_json::to_string(&WatcherPref { installed }) {
            let tmp = path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

#[derive(Serialize)]
struct SystemHealth {
    /// "ok" | "missing" | "stale-deleted" | "stale-other-install" |
    /// "stale-runlevel" | "disabled" | "no-script" | "unknown" - or several of
    /// the stale/disabled ones joined with commas, in the order the notice
    /// lists them.
    ///
    /// The three `stale-*` values share a fix (Repair re-registers the task
    /// with this copy's watcher.ps1 and RunLevel Limited) but not a cause, and
    /// the notice has to name the cause: "points at a deleted folder" would
    /// send the user looking for a deleted folder when the task is simply
    /// running a portable copy's watcher.
    watcher: String,
    /// "ok" | "missing" | "blocked" | "unknown"
    firewall: String,
    needs_watcher_fix: bool,
    needs_firewall_fix: bool,
}

#[cfg(windows)]
#[tauri::command]
async fn system_health(app: tauri::AppHandle) -> SystemHealth {
    // In the Store package Windows owns both pieces (see is_store_package):
    // there is no task or rule of ours to find, and nothing Repair could do.
    if is_store_package() {
        return SystemHealth {
            needs_watcher_fix: false,
            needs_firewall_fix: false,
            watcher: "store".to_string(),
            firewall: "store".to_string(),
        };
    }
    let exe = env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    // The watcher.ps1 that THIS copy should be running. Without it, the
    // check below can only ask "is there any file there?".
    let wanted_script = watcher_script_path()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    // Single-quoted inside the script, so a profile name with `$` or `"`
    // cannot break it; the only character to escape is the quote itself.
    let wanted_script = wanted_script.replace('\'', "''");
    let script = format!(
        r#"
# The same file written two ways is the same file: compared as text, a task
# registered with a short (8.3) path, a forward slash or a doubled backslash
# would be reported as "another copy of Mirryl". Get-Item gives the
# canonical long path; if the file is gone, GetFullPath at least normalises
# the separators.
function Norm($p) {{
    if (-not $p) {{ return "" }}
    try {{ return (Get-Item -LiteralPath $p -ErrorAction Stop).FullName }}
    catch {{ try {{ return [IO.Path]::GetFullPath($p) }} catch {{ return $p }} }}
}}
$task = Get-ScheduledTask -TaskName "{task}" -ErrorAction SilentlyContinue
$wanted = '{wanted_script}'
if (-not $wanted) {{
    # This copy has no watcher.ps1 beside Mirryl.exe (someone copied the exe
    # alone). Repair would only fail with "watcher.ps1 was not found next to
    # this app", so say the real thing instead of promising a repair.
    Write-Output "WATCHER=no-script"
}} elseif (-not $task) {{
    Write-Output "WATCHER=missing"
}} else {{
    $arguments = ($task.Actions | Select-Object -First 1).Arguments
    $match = [regex]::Match($arguments, '-File\s+"?([^"]+)"?')
    $scriptPath = ""
    if ($match.Success) {{ $scriptPath = $match.Groups[1].Value.Trim() }}
    # Every cause that applies, not just the first: a task can be disabled
    # AND point at another copy AND run elevated. Repair fixes them all in
    # one go, but the notice has to be true about what it found. Order is
    # the one the notice reads best in.
    $causes = @()
    if ("$($task.State)" -eq "Disabled") {{ $causes += "disabled" }}
    if ($scriptPath -and -not (Test-Path -LiteralPath $scriptPath)) {{
        $causes += "stale-deleted"
    }} elseif ($scriptPath -and ((Norm $scriptPath) -ne (Norm $wanted))) {{
        # The task runs ANOTHER install's watcher.ps1. The file exists, so
        # without this check the app would report everything as perfect and
        # never offer Repair while an old copy's watcher keeps running.
        $causes += "stale-other-install"
    }}
    if ("$($task.Principal.RunLevel)" -eq "Highest") {{
        # Privilege escalation: a task registered with the highest run level,
        # which watcher.ps1 does not need at all (it touches no firewall,
        # registry or services), stays that way until something registers it
        # again, and Repair is exactly what does.
        $causes += "stale-runlevel"
    }}
    if ($causes.Count -eq 0) {{ Write-Output "WATCHER=ok" }}
    else {{ Write-Output ("WATCHER=" + ($causes -join ",")) }}
}}

$inbound = $false
$outbound = $false
$blocked = $false
try {{
    $filters = Get-NetFirewallApplicationFilter -Program "{exe}" -ErrorAction SilentlyContinue
    foreach ($f in $filters) {{
        foreach ($rule in ($f | Get-NetFirewallRule -ErrorAction SilentlyContinue)) {{
            if ("$($rule.Enabled)" -ne "True") {{ continue }}
            if ("$($rule.Action)" -eq "Block") {{ $blocked = $true }}
            elseif ("$($rule.Direction)" -eq "Inbound") {{ $inbound = $true }}
            elseif ("$($rule.Direction)" -eq "Outbound") {{ $outbound = $true }}
        }}
    }}
}} catch {{ }}
if ($blocked) {{ Write-Output "FIREWALL=blocked" }}
elseif ($inbound -and $outbound) {{ Write-Output "FIREWALL=ok" }}
else {{ Write-Output "FIREWALL=missing" }}
"#,
        task = WATCHER_TASK_NAME,
        exe = exe,
        wanted_script = wanted_script
    );
    let out = run_powershell(&script);
    let field = |key: &str| -> String {
        out.lines()
            .find_map(|line| line.trim().strip_prefix(key).map(|v| v.trim().to_string()))
            .unwrap_or_else(|| "unknown".to_string())
    };
    let watcher = field("WATCHER=");
    let firewall = field("FIREWALL=");
    // "disabled,stale-runlevel" is one task with two things wrong; every
    // cause counts on its own.
    let causes: Vec<&str> = watcher.split(',').map(str::trim).collect();
    let has_any = |set: &[&str]| causes.iter().any(|c| set.contains(c));

    // Seeing the task on disk IS the answer to "did anyone ever install it" -
    // more reliable than our own file, which a task registered by the
    // installer never wrote.
    if has_any(&["ok", "stale-deleted", "stale-other-install", "stale-runlevel", "disabled"]) {
        set_watcher_wanted(&app, true);
    }
    let wanted = watcher_wanted(&app);

    // "no-script" is on the list so the notice shows; the text for it says
    // Repair cannot help (systemNoticeProblems in main.js).
    let needs_watcher_fix = wanted
        && has_any(&[
            "missing",
            "stale-deleted",
            "stale-other-install",
            "stale-runlevel",
            "disabled",
            "no-script",
        ]);
    SystemHealth {
        needs_watcher_fix,
        needs_firewall_fix: matches!(firewall.as_str(), "missing" | "blocked"),
        watcher,
        firewall,
    }
}

#[cfg(not(windows))]
#[tauri::command]
async fn system_health(_app: tauri::AppHandle) -> SystemHealth {
    // The watcher is a Windows scheduled task and the firewall rules are Windows
    // Firewall rules; there is nothing to check anywhere else.
    SystemHealth {
        watcher: "ok".to_string(),
        firewall: "ok".to_string(),
        needs_watcher_fix: false,
        needs_firewall_fix: false,
    }
}

/// Repairs whichever pieces the caller asks for, in ONE elevated run. Asking for
/// both and prompting twice would teach people to click through UAC dialogs
/// without reading them, which is the opposite of what this is for.
#[cfg(windows)]
#[tauri::command]
async fn repair_system(app: tauri::AppHandle, watcher: bool, firewall: bool) -> String {
    if !watcher && !firewall {
        return "Done. Nothing needed repairing.".to_string();
    }
    if is_store_package() {
        return STORE_FIREWALL_NOTE.to_string();
    }
    let mut script = String::new();
    if watcher {
        match watcher_script_path() {
            Some(path) => script.push_str(&watcher_repair_script(&path.to_string_lossy())),
            None => {
                return "Error: watcher.ps1 was not found next to this app. Make sure it's in the same folder as Mirryl.exe.".to_string()
            }
        }
    }
    if firewall {
        script.push_str(&firewall_repair_script());
    }

    let out = run_powershell_elevated(&script);
    if is_elevated_early_return(&out) {
        return out;
    }
    if out.contains("FAILED") {
        return format!("Error: {}", out);
    }
    if watcher {
        set_watcher_wanted(&app, true);
    }
    format!("Done. {}", out.replace('\n', " "))
}

#[cfg(not(windows))]
#[tauri::command]
async fn repair_system(_app: tauri::AppHandle, _watcher: bool, _firewall: bool) -> String {
    "Done. Nothing to repair on this platform.".to_string()
}

#[cfg(windows)]
#[tauri::command]
async fn install_watcher(app: tauri::AppHandle) -> String {
    // The watcher only ever reconnects wireless devices - a cable needs no
    // watching - so it belongs to the same tier as Wi-Fi.
    // Store package: the manifest's startup task already runs the watcher at
    // every sign-in. Start it now as well, so pairing works the same day
    // instead of after the next sign-in - the job Start-ScheduledTask does in
    // watcher_repair_script.
    if is_store_package() {
        set_watcher_wanted(&app, true);
        return match start_store_watcher() {
            Ok(()) => STORE_WATCHER_NOTE.to_string(),
            Err(e) => format!("Error: {e}"),
        };
    }
    let watcher_path = match watcher_script_path() {
        Some(p) => p,
        None => {
            return "ERROR: watcher.ps1 was not found next to this app. Make sure it's in the same folder as this app.".to_string()
        }
    };
    let out = run_powershell_elevated(&watcher_repair_script(&watcher_path.to_string_lossy()));
    if is_elevated_early_return(&out) {
        // Deliberately NOT an error. Declining a background convenience must not
        // cost the user the device they just spent two steps pairing - setup
        // finishes, and the notice on the device list offers it again later.
        //
        // Recorded as wanted anyway: they ran setup, which is the asking. "Not
        // now" on the notice is what turns it off, and only for this session.
        set_watcher_wanted(&app, true);
        return out;
    }
    if out.contains("FAILED") {
        return format!("Error: {}", out);
    }
    set_watcher_wanted(&app, true);
    format!("Done. {}", out)
}

#[cfg(not(windows))]
#[tauri::command]
async fn install_watcher(_app: tauri::AppHandle) -> String {
    // The auto-reconnect watcher is a Windows scheduled task; on other
    // platforms there is simply nothing to install. Report success (the
    // "Done." prefix is what the UI checks) so finishing setup isn't blocked.
    "Done. The auto-reconnect watcher is Windows-only; nothing to install on this platform.".to_string()
}

// ---------- Device tools (support toolkit) ----------
// For someone giving support on the same network - IT on another floor of the
// same building. Grouped by RISK, not by category: what only reads, what writes
// a file, and what interrupts the phone. Nothing destructive lives here yet;
// that tier (clear app data, uninstall, change resolution) needs its own
// confirmation design and is deliberately not built.

/// Keeps a LEGACY device folder in step with the alias that names it.
///
/// Mirryl writes nothing under Documents\Mirryl. What can still be there are
/// files an older version left, and `delete_tools_folder` - the "also delete
/// its saved files"
/// checkbox when removing a device - is how they get cleaned up. This keeps
/// them findable under the name the device has now, so that checkbox still
/// finds them after a rename. Best effort: only acts when the old folder
/// exists and nothing already sits at the new name, and never blocks the
/// rename itself if it fails.
fn rename_tools_output_dir(old_alias: &str, new_alias: &str) {
    let Some(base) = env::var_os("USERPROFILE").map(PathBuf::from) else {
        return;
    };
    let base = base.join("Documents").join("Mirryl");
    let old_dir = base.join(sanitize_for_filename(old_alias));
    let new_dir = base.join(sanitize_for_filename(new_alias));
    if old_dir == new_dir || !old_dir.is_dir() || new_dir.exists() {
        return;
    }
    let _ = std::fs::rename(&old_dir, &new_dir);
}

/// One line per action taken, in `%LOCALAPPDATA%\Mirryl\activity.log` - the
/// folder every other log already lives in (watcher.log, the engine's).
///
/// For a department, being able to say who touched what and when is the
/// difference between a tool that gets approved and one that gets banned.
/// Best-effort: never let logging break the action it is logging.
///
/// Same folder and same cap as watcher.log (2 MB, trimmed to its last 500
/// lines), and removed by the uninstaller.
fn log_activity(line: &str) {
    let Some(dir) = local_data_dir() else {
        return;
    };
    let path = dir.join("activity.log");
    let stamp = now_secs();
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write as _;
        let _ = writeln!(file, "[{stamp}] {line}");
    }
    trim_log_if_large(&path);
}

/// The same rule watcher.ps1 applies to its own log: past 2 MB, keep the
/// most recent 500 lines. Checked after every write, which costs one
/// metadata read; the rewrite itself happens once per 2 MB.
const ACTIVITY_LOG_MAX_BYTES: u64 = 2 * 1024 * 1024;
const ACTIVITY_LOG_KEEP_LINES: usize = 500;

pub(crate) fn trim_log_if_large(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() <= ACTIVITY_LOG_MAX_BYTES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let _ = std::fs::write(path, keep_last_lines(&text, ACTIVITY_LOG_KEEP_LINES));
}

/// The last `keep` lines of `text`, each ending in a newline (pure, tested).
fn keep_last_lines(text: &str, keep: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(keep);
    let mut out = String::new();
    for line in &lines[start..] {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Deletes the activity log that older versions kept under
/// `Documents\Mirryl`, and that folder when nothing else is left in it. Only
/// that one file: anything else there (an old bug report, something the user
/// put there) is theirs. Best-effort, once per start.
fn remove_legacy_activity_log() {
    let Some(base) = env::var_os("USERPROFILE").map(PathBuf::from) else {
        return;
    };
    let dir = base.join("Documents").join("Mirryl");
    let old = dir.join("activity.log");
    if !old.is_file() {
        return;
    }
    if std::fs::remove_file(&old).is_ok() {
        log_activity("moved the activity log out of Documents\\Mirryl (the old file was deleted)");
    }
    // `remove_dir` (not `_all`) refuses a folder that still has anything in
    // it, which is exactly the guarantee wanted here.
    let _ = std::fs::remove_dir(&dir);
}

// ---- pure parsers, so the fragile part is the part that is tested ----

fn parse_battery_level(dumpsys: &str) -> Option<u8> {
    dumpsys
        .lines()
        .find_map(|line| line.trim().strip_prefix("level:"))
        .and_then(|value| value.trim().parse().ok())
}

/// `df /data` in 1K blocks. Column 3 is what is Available - not column 2, which
/// is what is used, and getting that backwards would report a full phone as
/// empty.
fn parse_free_space_kb(df_output: &str) -> Option<u64> {
    df_output.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() >= 4 {
            fields[3].parse().ok()
        } else {
            None
        }
    })
}

fn format_gb(kilobytes: u64) -> String {
    format!("{:.1} GB", kilobytes as f64 / 1024.0 / 1024.0)
}

/// `ip route` prints `... scope link src 192.168.0.4`. Read the address off
/// that rather than from a wlan0-specific command: the interface name is not
/// the same on every phone, the route is.
fn parse_ip_from_route(route_output: &str) -> Option<String> {
    let mut fields = route_output.split_whitespace().peekable();
    while let Some(field) = fields.next() {
        if field == "src" {
            if let Some(address) = fields.next() {
                if address.split('.').count() == 4 {
                    return Some(address.to_string());
                }
            }
        }
    }
    None
}

/// `/proc/uptime` starts with the seconds the phone has been awake.
fn format_uptime(proc_uptime: &str) -> Option<String> {
    let seconds: f64 = proc_uptime.split_whitespace().next()?.parse().ok()?;
    let total = seconds as u64;
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    Some(if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    })
}

#[derive(Serialize)]
struct DeviceInfo {
    battery: String,
    storage: String,
    android: String,
    model: String,
    serial: String,
    uptime: String,
    ip: String,
    /// False when adb could not reach the phone at all, so the UI can say
    /// "not reachable" instead of showing seven dashes and looking broken.
    reachable: bool,
}

#[tauri::command]
async fn device_info(target: String) -> Result<DeviceInfo, String> {
    let adb = adb_path();
    let dash = "—".to_string();
    let shell = |args: &[&str]| -> String {
        let mut full = vec!["-s", target.as_str(), "shell"];
        full.extend_from_slice(args);
        run_process(&adb, &full).trim().to_string()
    };

    let model = shell(&["getprop", "ro.product.model"]);
    // One reachable answer is the test. Asking `adb devices` instead would say
    // the transport exists while the phone is already gone.
    if model.is_empty() || adb_says_gone(&model) {
        return Ok(DeviceInfo {
            battery: dash.clone(),
            storage: dash.clone(),
            android: dash.clone(),
            model: dash.clone(),
            serial: dash.clone(),
            uptime: dash.clone(),
            ip: dash,
            reachable: false,
        });
    }

    let battery = parse_battery_level(&shell(&["dumpsys", "battery"]))
        .map(|level| format!("{level}%"))
        .unwrap_or_else(|| dash.clone());
    let storage = parse_free_space_kb(&shell(&["df", "/data"]))
        .map(format_gb)
        .map(|free| format!("{free} free"))
        .unwrap_or_else(|| dash.clone());
    let android = shell(&["getprop", "ro.build.version.release"]);
    let serial = shell(&["getprop", "ro.serialno"]);
    let uptime = format_uptime(&shell(&["cat", "/proc/uptime"])).unwrap_or_else(|| dash.clone());
    let ip = parse_ip_from_route(&shell(&["ip", "route"])).unwrap_or_else(|| dash.clone());

    Ok(DeviceInfo {
        battery,
        storage,
        android: if android.is_empty() { dash.clone() } else { format!("Android {android}") },
        model,
        serial: if serial.is_empty() { dash.clone() } else { serial },
        uptime,
        ip,
        reachable: true,
    })
}

#[tauri::command]
async fn device_reboot(target: String, alias: String) -> String {
    let output = run_process(&adb_path(), &["-s", &target, "reboot"]);
    log_activity(&format!("reboot sent to {alias} ({target})"));
    if adb_says_gone(&output) {
        return format!("Error: {}", output.trim());
    }
    // Deliberately not "done": the phone is GONE for the next half minute and
    // the UI has to say so, or it looks like the app broke.
    "Rebooting. The phone will disappear for about half a minute and come back on its own.".to_string()
}

/// True when adb's own output says the phone isn't really there, in any of
/// the wordings actually seen from adb.exe on Windows: `error: device 'x'
/// not found` (fully gone), `adb.exe: device offline` (transport still
/// registered but dead - the common case for a Wi-Fi session that dropped
/// without a clean close, and notably NOT prefixed with "error:"), or a
/// local timeout message (transport hung instead of erroring at all).
/// Deliberately does not treat an empty string as "gone" - `adb reboot`
/// prints nothing on success, and that emptiness must stay a success there.
fn adb_says_gone(output: &str) -> bool {
    output.contains("not found")
        || output.contains("error:")
        || output.contains("offline")
        || output.contains("Timed out")
}

/// Where Mirryl lives: its repository on GitHub. The only address the app
/// ever opens, and nothing is fetched from it - it is a link for the user.
const REPOSITORY_URL: &str = "https://github.com/johan-socarras/Mirryl";

/// Opens Mirryl's repository in whatever the user set as their default
/// browser.
///
/// The address is a constant here rather than a parameter on purpose: the
/// frontend can ask for "the repository" and nothing else, so no page loaded
/// in the webview can talk this command into launching an arbitrary URL.
#[tauri::command]
fn open_repository() -> String {
    let site = REPOSITORY_URL;
    #[cfg(windows)]
    {
        // Same idiom as the folder openers below: handing a URL to
        // explorer.exe launches the default browser, which saves pulling in
        // a shell/opener plugin for this single link.
        let mut command = Command::new("explorer.exe");
        command.arg(site);
        let _ = command.spawn();
    }
    #[cfg(not(windows))]
    {
        open_with_desktop(site.as_ref());
    }
    format!("Done. {site}")
}

/// Linux: hands a URL or a folder to the desktop (`xdg-open`), and reaps it
/// when it is done so it does not linger as a zombie of this app.
#[cfg(not(windows))]
fn open_with_desktop(target: &std::ffi::OsStr) {
    if let Ok(mut child) = Command::new("xdg-open").arg(target).spawn() {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Opens Explorer with `filename` (expected in the app's folder, `app_dir`)
/// pre-selected, so the user lands directly on the file instead of a bare
/// folder view. Shared by the two "show in folder" links of the Licenses
/// screen. On Linux the file manager opens on its folder: there is no one
/// way to pre-select a file that every desktop understands.
fn reveal_file_next_to_exe(filename: &str) -> String {
    let Some(dir) = app_dir() else {
        return "Error: could not locate the application folder.".to_string();
    };
    let target = dir.join(filename);
    if !target.exists() {
        return format!("Error: {filename} was not found next to this app.");
    }
    #[cfg(windows)]
    {
        // /select, and the path must travel as ONE argument - explorer.exe
        // does not recognize them as a pair split across two argv entries.
        let mut select_arg = std::ffi::OsString::from("/select,");
        select_arg.push(target.as_os_str());
        let mut command = Command::new("explorer.exe");
        command.arg(select_arg);
        let _ = command.spawn();
    }
    #[cfg(not(windows))]
    {
        open_with_desktop(dir.as_os_str());
    }
    format!("Done. {}", target.display())
}

/// The MPL 2.0 covers option-ext, the one linked component that offers no
/// license but MPL-2.0 (see Section 2.4 of THIRD_PARTY_LICENSES.txt), and
/// requires its source be made available. Bundling the actual source next
/// to the app, instead of only pointing at crates.io, means that
/// requirement holds even if that page ever moves or disappears.
#[tauri::command]
fn open_mpl_source_folder() -> String {
    reveal_file_next_to_exe("mpl-source-code.zip")
}

/// COMPONENT-INVENTORY.txt carries the per-component copyright notices
/// that THIRD_PARTY_LICENSES.txt itself points to rather than repeats -
/// it is part of the license notice, not a separate optional document.
#[tauri::command]
fn open_component_inventory_folder() -> String {
    reveal_file_next_to_exe("COMPONENT-INVENTORY.txt")
}

/// Same "show in folder" pattern as the two license links above, for the
/// EULA (Settings screen).
#[tauri::command]
fn open_eula_folder() -> String {
    reveal_file_next_to_exe("EULA.txt")
}

/// Same pattern, for the privacy notice.
#[tauri::command]
fn open_privacy_folder() -> String {
    reveal_file_next_to_exe("PRIVACY.txt")
}

/// Opens the user guide (a PDF beside the app) in whatever opens PDFs here.
#[tauri::command]
fn open_user_guide() -> String {
    let Some(path) = app_dir().map(|dir| dir.join("Mirryl-User-Guide.pdf")) else {
        return "Error: could not locate the application folder.".to_string();
    };
    if !path.is_file() {
        return "Error: Mirryl-User-Guide.pdf was not found next to this app. Reinstall Mirryl to get it back.".to_string();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("explorer.exe").arg(path.as_os_str()).spawn();
    }
    #[cfg(not(windows))]
    {
        open_with_desktop(path.as_os_str());
    }
    format!("Done. {}", path.display())
}

/// Explicit, user-requested deletion of a device's Diagnostics folder -
/// only ever called right after the user checks "also delete its saved
/// files" in the Remove device confirmation. Never automatic.
#[tauri::command]
fn delete_tools_folder(alias: String) -> String {
    let Some(base) = env::var_os("USERPROFILE").map(PathBuf::from) else {
        return "Error: could not resolve the user profile folder.".to_string();
    };
    let dir = base.join("Documents").join("Mirryl").join(sanitize_for_filename(&alias));
    if !dir.is_dir() {
        return "Done.".to_string();
    }
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => {
            log_activity(&format!("deleted diagnostics folder for {alias}"));
            "Done.".to_string()
        }
        Err(e) => format!("Error: could not delete the folder: {e}"),
    }
}

/// What the INSTALLER asks of us, already elevated: create the watcher task
/// and the five firewall rules.
///
/// Why it exists, instead of the installer carrying the commands inside:
/// NSIS cuts any string at 1024 characters (`NSIS_MAX_STRLEN` on the copy
/// Tauri uses), and the full recipe is longer: cut in half, PowerShell would
/// fail with a syntax error and return at once without creating anything.
///
/// Calling in here, the installer's command is a few dozen characters and
/// the recipe lives in ONE PLACE: the same code the Repair button uses.
/// Two copies of this to be kept identical by hand would be the kind of
/// duplicate that drifts the moment someone renames a rule.
///
/// Returns `None` when we were not called this way, and the app starts
/// normally.
///
/// Does NOT elevate again: the installer already launched us elevated, and
/// asking again would be a second UAC for the same work.
#[cfg(windows)]
fn handle_setup_repair_argument() -> Option<i32> {
    let mut args = env::args().skip(1);
    if args.next()? != "--repair-setup" {
        return None;
    }
    // The user comes from the installer WITHOUT elevation, which is the only
    // one that knows who is installing. Empty -> the account running this,
    // which is right when that person already is an administrator.
    let user = args.next().unwrap_or_default();
    let user_expr = if user.trim().is_empty() {
        "$env:USERNAME".to_string()
    } else {
        format!("'{}'", user.replace('\'', "''"))
    };

    let Some(watcher) = watcher_script_path() else {
        eprintln!("FAILED: watcher.ps1 was not found next to Mirryl.exe");
        return Some(2);
    };

    let mut script = watcher_repair_script_for(&watcher.to_string_lossy(), &user_expr);
    script.push_str(&firewall_repair_script());
    let out = run_powershell(&script);
    let failed = out.contains("FAILED");

    // To the log, not the console: this is a windowed app and has none, so
    // nobody reads `println!`. The installer writes to this same file before
    // and after, so both halves of the story stay together and in order.
    let entry = format!(
        "app --repair-setup: user={} -> {}\r\n{}\r\n",
        if user.trim().is_empty() { "(current)" } else { user.trim() },
        if failed { "FAILED" } else { "ok" },
        out.trim()
    );
    if let Some(dir) = env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        use std::io::Write;
        let path = dir.join("setup.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(entry.as_bytes());
        }
        // The installer writes here too and cannot trim; the app does it for
        // both halves (past 2 MB, the last 500 lines).
        trim_log_if_large(&path);
    }

    Some(if failed { 1 } else { 0 })
}

fn main() {
    // First of all: the installer may be calling us to finish preparing the
    // PC, and then this is not an app being opened.
    #[cfg(windows)]
    if let Some(code) = handle_setup_repair_argument() {
        std::process::exit(code);
    }
    // Same for the Store package's start at sign-in: the manifest launches
    // `Mirryl.exe --watcher`, which starts the watcher and exits without
    // opening any window (see start_store_watcher).
    #[cfg(windows)]
    if env::args().nth(1).as_deref() == Some("--watcher") {
        if let Err(e) = start_store_watcher() {
            log_activity(&format!("--watcher: {e}"));
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    tauri::Builder::default()
        // Paint the title bar with the SAVED theme before the window is shown,
        // or every launch would start with a black bar and repaint itself once
        // the page got around to asking.
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let theme = get_theme(app.handle().clone());
                paint_caption(&window, &theme);
            }
            // Migrate pc_identity.json (1.0.0 clear text -> DPAPI + own DACL)
            // at launch, not only when a phone connects, so a PC that stops
            // connecting phones does not keep its private key in the clear.
            // Idempotent: a v2 file is read and left untouched. See
            // matriculation::migrate_identity_if_needed for why creating the
            // identity here, on a PC that never had one, is harmless.
            matriculation::migrate_identity_if_needed(app.handle());
            // Before the first line is logged: the log lives in
            // %LOCALAPPDATA%\Mirryl, and an old Documents\Mirryl copy goes.
            remove_legacy_activity_log();
            Ok(())
        })
        .manage(std::sync::Arc::new(qr_pairing::QrPairingState::default()))
        .invoke_handler(tauri::generate_handler![
            pair,
            connect_and_grant,
            install_phone_app,
            restart_adb,
            get_status,
            discover_port,
            get_third_party_licenses,
            open_repository,
            open_mpl_source_folder,
            open_component_inventory_folder,
            open_eula_folder,
            open_privacy_folder,
            open_user_guide,
            start_mirror,
            focus_mirror,
            list_open_mirrors,
            mirror_command,
            firewall_fix,
            install_watcher,
            system_health,
            repair_system,
            device_info,
            device_reboot,
            delete_tools_folder,
            list_devices,
            add_device,
            update_device,
            forget_wifi_address,
            remove_device,
            reconcile_devices,
            mdns_transports,
            device_serial,
            pick_file,
            push_file,
            get_theme,
            set_theme,
            get_keybindings,
            set_keybindings,
            app_version,
            qr_pairing::start_qr_pairing,
            qr_pairing::get_qr_pairing_status,
            qr_pairing::cancel_qr_pairing,
            apps::list_apps,
            apps::app_action,
            apps::play_store_search,
            apps::install_apks,
            apps::backup_apk,
            apps::pick_apks,
            apps::reinstall_phone_app,
            apps::set_lock_pin,
            apps::app_pin,
            apps::save_inventory,
            apps::get_profiles,
            apps::save_profiles,
            apps::apply_profile,
            apps::pin_alerts,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        connect_succeeded, ensure_adb_key, explain_install_failure, format_gb, format_uptime,
        is_ip_port, is_listed, is_present_in, is_wireless_transport, keep_last_lines,
        parse_adb_device_line, parse_battery_level, parse_free_space_kb, parse_installed_phone_app,
        parse_ip_from_route, parse_mdns_transports, parse_phone_app_version_file,
        parse_ready_wireless_devices, phone_app_state,
        pick_preferred_serial, seal_json, split_last_two_fields, upsert_device, Device,
        InstalledPhoneApp, PhoneAppState, PACKAGE_NAME,
    };

    /// An mDNS rename puts a SPACE inside the transport name. The two
    /// listings adb prints, plain (tab) and `-l` (padded with spaces).
    const ADB_RENAMED: &str = "List of devices attached\n\
        adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp\tdevice\n\
        emulator-5554\tdevice\n";
    const ADB_RENAMED_LONG: &str = "List of devices attached\n\
        adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp device product:m2quew model:SM_S947U1 device:m2q transport_id:1\n\
        emulator-5554          device product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emu64xa transport_id:4\n";

    /// What Mirror asks before starting the engine: the renamed phone is there.
    #[test]
    fn a_renamed_mdns_transport_is_still_present() {
        let name = "adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp";
        assert!(is_present_in(ADB_RENAMED, name));
        assert!(is_present_in(ADB_RENAMED_LONG, name));
        // Half the name is not the transport.
        assert!(!is_present_in(ADB_RENAMED, "adb-R5CX12ABCDE-62Sel0"));
        // Present but not usable is still "not connected".
        assert!(!is_present_in(
            "List of devices attached\nadb-X-1 (2)._adb-tls-connect._tcp\toffline\n",
            "adb-X-1 (2)._adb-tls-connect._tcp"
        ));
    }

    #[test]
    fn adb_device_lines_split_at_the_state_not_at_the_first_space() {
        assert_eq!(parse_adb_device_line("R5CX12ABCDE\tdevice"), Some(("R5CX12ABCDE", "device")));
        assert_eq!(
            parse_adb_device_line("adb-A-1 (2)._adb-tls-connect._tcp device product:p model:m device:d"),
            Some(("adb-A-1 (2)._adb-tls-connect._tcp", "device"))
        );
        // The trailing `device:m2q` field is not the state.
        assert_eq!(parse_adb_device_line("SER offline device:m2q"), Some(("SER", "offline")));
        // A state with a space in it.
        assert_eq!(
            parse_adb_device_line("ABC\tno permissions (user in plugdev group); see [x]"),
            Some(("ABC", "no permissions"))
        );
        assert_eq!(parse_adb_device_line("List of devices attached"), None);
        assert_eq!(parse_adb_device_line("* daemon started successfully"), None);
        assert_eq!(parse_adb_device_line(""), None);
        assert_eq!(parse_adb_device_line("device"), None);
    }

    /// The same rename in `adb mdns services`: the phone's CURRENT address is
    /// the one behind the renamed instance, and it must not be dropped.
    #[test]
    fn a_renamed_mdns_service_keeps_its_address() {
        let listing = "List of discovered mdns services\n\
            adb-R5CX12ABCDE-62Sel0 (2)\t_adb-tls-connect._tcp\t192.168.0.4:36613\n\
            adb-R5CX12ABCDE-62Sel0\t_adb-tls-connect._tcp\t192.168.0.4:34419\n";
        let found = parse_mdns_transports(listing);
        assert_eq!(
            found,
            vec![
                (
                    "adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp".to_string(),
                    "192.168.0.4:36613".to_string()
                ),
                ("adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp".to_string(), "192.168.0.4:34419".to_string()),
            ]
        );
        assert_eq!(split_last_two_fields("only two"), None);
        assert_eq!(split_last_two_fields("a b  c"), Some(("a", "b", "c")));
    }

    /// Matriculation on mirror is for phones in the list: a USB serial, an
    /// mDNS name carrying a listed serial, or a saved ip:port count; an
    /// unsaved cable phone does not.
    #[test]
    fn listed_means_a_row_matches_the_target() {
        let devices = vec![
            Device { id: 1, alias: "Phone 1".into(), host_port: "192.168.0.5:5555".into(), serial: Some("R5CX12ABCDE".into()), cable_only: false },
            Device { id: 2, alias: "Cable".into(), host_port: String::new(), serial: Some("AS01XYZ".into()), cable_only: false },
        ];
        assert!(is_listed(&devices, "R5CX12ABCDE"));
        assert!(is_listed(&devices, "adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp"));
        assert!(is_listed(&devices, "192.168.0.5:5555"));
        assert!(is_listed(&devices, "AS01XYZ"));
        assert!(!is_listed(&devices, "ELEVENTH"));
        assert!(!is_listed(&devices, ""));
    }

    /// activity.log past 2 MB keeps its last 500 lines, like watcher.log.
    #[test]
    fn keep_last_lines_keeps_the_tail_with_newlines() {
        let text: String = (1..=7).map(|n| format!("line {n}\n")).collect();
        assert_eq!(keep_last_lines(&text, 3), "line 5\nline 6\nline 7\n");
        assert_eq!(keep_last_lines(&text, 50), text);
        assert_eq!(keep_last_lines("", 3), "");
    }

    /// Pins seal_json's hash to an independently-computed reference
    /// (Python hashlib.sha256("mirryl-seal-v1-8f2c:1755000000")) so a change
    /// to the pepper, the separator, or the hex casing here cannot drift
    /// silently out of sync with WifiAdbHelper.java and watcher.ps1's
    /// Send-Seal, which both have to reproduce this exact hash independently
    /// with no shared code to catch a mismatch at compile time.
    #[test]
    fn seal_json_hash_matches_independent_reference() {
        let json = seal_json(1755000000);
        assert_eq!(
            json,
            "{\"v\":\"16c65c9b625689329f1db7e795e99006426fbfa445d8888b0ce5160b7feec04b\",\"ts\":1755000000}"
        );
    }

    /// Simulates a machine that never ran adb (fresh home with no .android
    /// dir): ensure_adb_key must create adbkey + adbkey.pub via `adb keygen`.
    /// Needs adb on PATH. Serialized by being the only test that touches the
    /// home-directory variables.
    ///
    /// It overrides USERPROFILE **and** HOME because adb_pubkey_path prefers
    /// USERPROFILE: with only HOME set, the function would find the
    /// developer's own key and return Ok without generating anything.
    #[test]
    fn ensure_adb_key_generates_key_on_fresh_machine() {
        let tmp = std::env::temp_dir().join(format!("mirryl-keytest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let old_home = std::env::var("HOME").ok();
        let old_profile = std::env::var("USERPROFILE").ok();
        std::env::set_var("HOME", &tmp);
        std::env::set_var("USERPROFILE", &tmp);

        let result = ensure_adb_key();

        let pubkey = tmp.join(".android").join("adbkey.pub");
        let exists = pubkey.exists();
        let contents = std::fs::read_to_string(&pubkey).unwrap_or_default();

        // Restore both before asserting so a failure can't poison other state.
        match old_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match old_profile {
            Some(p) => std::env::set_var("USERPROFILE", p),
            None => std::env::remove_var("USERPROFILE"),
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert!(result.is_ok(), "ensure_adb_key errored: {:?}", result);
        assert!(exists, "adbkey.pub was not generated");
        // adb pubkeys are base64 blobs ending with a " user@host" style comment.
        assert!(contents.trim().len() > 100, "adbkey.pub looks too short: {}", contents);
    }

    #[test]
    fn connect_succeeded_recognizes_success_and_failure() {
        assert!(connect_succeeded("connected to 192.168.0.4:43287"));
        assert!(connect_succeeded("already connected to 192.168.0.4:43287"));
        assert!(!connect_succeeded("failed to connect to '192.168.0.4:40375': Connection refused"));
        assert!(!connect_succeeded("cannot connect to ..."));
        assert!(!connect_succeeded(""));
    }

    #[test]
    fn ready_wireless_devices_keeps_only_the_device_state() {
        let output = "List of devices attached\n\
192.168.0.187:37171\tdevice product:C66 model:C66\n\
192.168.0.230:41161\toffline\n\
192.168.0.9:5555\tconnecting\n\
192.168.0.50:4444\tunauthorized\n\
adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp\tdevice\n\
6d16ad42\tdevice\n";
        assert_eq!(
            parse_ready_wireless_devices(output),
            vec!["192.168.0.187:37171".to_string()]
        );
    }

    fn dev(id: u64, alias: &str, host_port: &str, serial: Option<&str>) -> Device {
        Device {
            id,
            alias: alias.to_string(),
            host_port: host_port.to_string(),
            serial: serial.map(|s| s.to_string()),
            cable_only: false,
        }
    }

    #[test]
    fn a_number_that_is_not_a_yes_or_no_fails_instead_of_being_guessed() {
        for text in ["1.0", "2.7", "-0.4", "9223372036854775808"] {
            let json = format!(r#"[{{"id":1,"alias":"A","host_port":"","serial":"S","cable_only":{text}}}]"#);
            assert!(
                serde_json::from_str::<Vec<Device>>(&json).is_err(),
                "{text} should not be read as a yes/no value"
            );
        }
        // Whole numbers still answer.
        for (text, expected) in [("1", true), ("0", false), ("-1", true)] {
            let json = format!(r#"[{{"id":1,"alias":"A","host_port":"","serial":"S","cable_only":{text}}}]"#);
            let rows: Vec<Device> = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(rows[0].cable_only, expected, "{text}");
        }
    }

    #[test]
    fn a_hand_typed_cable_only_does_not_empty_the_device_list() {
        for text in [r#""true""#, r#""TRUE""#, "1", r#""yes""#] {
            let json = format!(r#"[{{"id":1,"alias":"A","host_port":"","serial":"S","cable_only":{text}}}]"#);
            let rows: Vec<Device> = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(rows.len(), 1, "{text}");
            assert!(rows[0].cable_only, "{text}");
        }
        for text in [r#""false""#, "0", "null"] {
            let json = format!(r#"[{{"id":1,"alias":"A","host_port":"","serial":"S","cable_only":{text}}}]"#);
            let rows: Vec<Device> = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert!(!rows[0].cable_only, "{text}");
        }
        // Gibberish still fails rather than being guessed at.
        assert!(serde_json::from_str::<Vec<Device>>(
            r#"[{"id":1,"alias":"A","host_port":"","serial":"S","cable_only":"perhaps"}]"#
        )
        .is_err());
    }

    #[test]
    fn only_a_plain_ip_port_is_handed_to_adb_disconnect() {
        assert!(is_ip_port("192.168.0.187:37171"));
        assert!(!is_ip_port("adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp"));
        assert!(!is_ip_port("adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp"));
        assert!(!is_ip_port("6d16ad42"));
        assert!(!is_ip_port(""));
        assert!(!is_ip_port("192.168.0.187"));
        assert!(!is_ip_port("192.168.0.187:"));
        assert!(!is_ip_port("host.local:5555"));
    }

    #[test]
    fn forgetting_wifi_survives_a_save_and_pairing_clears_it() {
        // The flag rides the JSON both ways, and is absent from a row that
        // does not use it - devices.json keeps the shape it always had.
        let mut devices = vec![dev(1, "C66", "192.168.0.230:41161", Some("0a1b2c3d"))];
        devices[0].cable_only = true;
        devices[0].host_port = String::new();
        let json = serde_json::to_string(&devices).expect("serialises");
        assert!(json.contains("\"cable_only\":true"), "{json}");
        let back: Vec<Device> = serde_json::from_str(&json).expect("parses");
        assert!(back[0].cable_only);

        let plain = serde_json::to_string(&vec![dev(2, "AS01", "", Some("6d16ad42"))]).expect("ok");
        assert!(!plain.contains("cable_only"), "{plain}");
        // And a file without the field still loads.
        let old: Vec<Device> =
            serde_json::from_str(r#"[{"id":1,"alias":"A","host_port":"","serial":"S"}]"#).expect("old file");
        assert!(!old[0].cable_only);

        // Pairing it over Wi-Fi again is what turns the flag off.
        let mut devices = back;
        upsert_device(&mut devices, "C66".into(), "192.168.0.187:37171".into(), Some("0a1b2c3d".into()));
        assert_eq!(devices.len(), 1);
        assert!(!devices[0].cable_only);
        assert_eq!(devices[0].host_port, "192.168.0.187:37171");
    }

    #[test]
    fn adds_a_new_device_when_unseen() {
        let mut devices = vec![dev(1, "A", "192.168.0.4:35231", Some("SERA"))];
        upsert_device(&mut devices, "B".into(), "192.168.0.9:41000".into(), Some("SERB".into()));
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].id, 2);
        assert_eq!(devices[1].serial.as_deref(), Some("SERB"));
    }

    #[test]
    fn re_pairing_same_serial_updates_in_place_even_if_ip_changed() {
        // Serial is the point: same phone, DHCP gave it a new IP AND a new
        // port -> still one row, id + alias preserved.
        let mut devices = vec![dev(7, "My Phone", "192.168.0.4:35231", Some("R5CX12ABCDE"))];
        upsert_device(&mut devices, "Device (10.0.0.2)".into(), "10.0.0.2:44835".into(), Some("R5CX12ABCDE".into()));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 7);
        assert_eq!(devices[0].alias, "My Phone");
        assert_eq!(devices[0].host_port, "10.0.0.2:44835");
    }

    #[test]
    fn migrates_ip_only_entry_by_stamping_serial() {
        // Older entry has no serial; re-pairing matches by IP and stamps it.
        let mut devices = vec![dev(3, "Device (192.168.0.4)", "192.168.0.4:35231", None)];
        upsert_device(&mut devices, "Device (192.168.0.4)".into(), "192.168.0.4:50000".into(), Some("R5CX12ABCDE".into()));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 3);
        assert_eq!(devices[0].serial.as_deref(), Some("R5CX12ABCDE"));
        assert_eq!(devices[0].host_port, "192.168.0.4:50000");
    }

    #[test]
    fn collapses_existing_duplicates_for_the_same_phone() {
        // Two IP-only rows for one phone; a serial-carrying re-pair merges them.
        let mut devices = vec![
            dev(1, "Device (192.168.0.4)", "192.168.0.4:35231", None),
            dev(2, "Device (192.168.0.4)", "192.168.0.4:44835", None),
        ];
        upsert_device(&mut devices, "Device (192.168.0.4)".into(), "192.168.0.4:50000".into(), Some("R5CX12ABCDE".into()));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 1);
        assert_eq!(devices[0].host_port, "192.168.0.4:50000");
        assert_eq!(devices[0].serial.as_deref(), Some("R5CX12ABCDE"));
    }

    #[test]
    fn falls_back_to_ip_when_no_serial_known() {
        // No serial available (discovery failed) -> IP dedup.
        let mut devices = vec![dev(1, "A", "192.168.0.4:35231", None)];
        upsert_device(&mut devices, "A".into(), "192.168.0.4:60000".into(), None);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].host_port, "192.168.0.4:60000");
    }

    #[test]
    fn keeps_distinct_phones_separate() {
        let mut devices = vec![dev(1, "A", "192.168.0.4:35231", Some("SERA"))];
        upsert_device(&mut devices, "B".into(), "192.168.0.4:41000".into(), Some("SERB".into()));
        // Same IP but different serial => a genuinely different phone => 2 rows.
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].serial.as_deref(), Some("SERB"));
    }

    /// A phone added over the cable carries no ip:port. Adding one that is
    /// ALREADY registered for Wi-Fi must not overwrite the address it already
    /// has - otherwise plugging a cable in silently costs the user the ability
    /// to use that phone without one.
    #[test]
    fn adding_over_usb_keeps_a_known_wireless_address() {
        let mut devices = vec![dev(1, "Phone", "192.168.0.4:40701", Some("R5CX12ABCDE"))];
        upsert_device(&mut devices, "Phone (USB)".into(), String::new(), Some("R5CX12ABCDE".into()));
        assert_eq!(devices.len(), 1, "same phone, must not add a second row");
        assert_eq!(devices[0].host_port, "192.168.0.4:40701");
        assert_eq!(devices[0].alias, "Phone", "a user-set alias survives");
    }

    /// The other direction: a phone first met over the cable, later paired for
    /// Wi-Fi, gains its address in place instead of appearing twice.
    #[test]
    fn a_usb_device_gains_its_wireless_address_when_paired() {
        let mut devices = vec![dev(1, "Phone", "", Some("R5CX12ABCDE"))];
        upsert_device(&mut devices, "Phone".into(), "192.168.0.4:40701".into(), Some("R5CX12ABCDE".into()));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 1, "still the same row the user set up");
        assert_eq!(devices[0].host_port, "192.168.0.4:40701");
    }

    /// After a reboot the phone comes back under its mDNS name with a NEW port,
    /// so nothing in `adb devices` looks like the saved address. Joining the
    /// name to the address mDNS reports is the only bridge back.
    #[test]
    fn joins_mdns_names_to_the_address_they_live_at() {
        let listing = "List of discovered mdns services
            adb-R5CX12ABCDE-62Sel0	_adb-tls-connect._tcp	192.168.0.4:39509
            adb-R5CX12ABCDE-62Sel0	_adb-tls-pairing._tcp	192.168.0.4:41235
";
        let found = parse_mdns_transports(listing);
        // The pairing service is a DIFFERENT port that refuses connections;
        // handing it out would produce an address nothing can talk to.
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, "adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp");
        assert_eq!(found[0].1, "192.168.0.4:39509");
    }

    #[test]
    fn ignores_lines_that_are_not_services() {
        assert!(parse_mdns_transports("List of discovered mdns services
").is_empty());
        assert!(parse_mdns_transports("").is_empty());
        assert!(parse_mdns_transports("junk	_adb-tls-connect._tcp	no-port").is_empty());
    }

    // ---- Device tools: the parsers, which is where this breaks ----

    #[test]
    fn reads_the_battery_level_out_of_dumpsys() {
        let dumpsys = "Current Battery Service state:
  AC powered: false
  level: 84
  scale: 100
";
        assert_eq!(parse_battery_level(dumpsys), Some(84));
        assert_eq!(parse_battery_level("nothing useful here"), None);
    }

    /// The column that matters is Available (index 3), NOT Used (index 2).
    /// Getting those two backwards would report a full phone as empty, which is
    /// worse than showing nothing.
    #[test]
    fn reads_the_free_space_column_not_the_used_one() {
        let df = "Filesystem     1K-blocks      Used Available Use% Mounted on
                  /dev/block/dm-9 111550000  98000000  13550000  88% /data
";
        assert_eq!(parse_free_space_kb(df), Some(13_550_000));
        assert_eq!(format_gb(13_550_000), "12.9 GB");
        assert_eq!(parse_free_space_kb("Filesystem 1K-blocks
"), None);
    }

    /// Read off the route, not off a wlan0-specific command: the interface name
    /// is not the same on every phone, the route is.
    #[test]
    fn reads_the_phone_ip_off_the_route_table() {
        let route = "192.168.0.0/24 dev wlan0 proto kernel scope link src 192.168.0.4
                     default via 192.168.0.1 dev wlan0 table 1021 proto static
";
        assert_eq!(parse_ip_from_route(route), Some("192.168.0.4".to_string()));
        assert_eq!(parse_ip_from_route("no routes at all"), None);
    }

    #[test]
    fn formats_uptime_by_how_long_it_actually_is() {
        assert_eq!(format_uptime("312450.12 1200.4").as_deref(), Some("3d 14h"));
        assert_eq!(format_uptime("7300.0 1.0").as_deref(), Some("2h 1m"));
        assert_eq!(format_uptime("300.0 1.0").as_deref(), Some("5m"));
        assert_eq!(format_uptime("").as_deref(), None);
        assert_eq!(format_uptime("not-a-number").as_deref(), None);
    }

    /// Every shape adb can hand us is pinned here. The mDNS name is the trap - it carries no colon, so a
    /// naive "contains ':'" test would wave a wireless link straight through.
    #[test]
    fn wireless_transports_are_told_apart_from_usb() {
        assert!(is_wireless_transport("192.168.0.4:40701"));
        assert!(is_wireless_transport("adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp"));
        // The rename mDNS applies on a name collision. The space and the "(2)"
        // sit in the MIDDLE, so a test that only looked at the end of the
        // string would pass while treating this wireless link as a cable.
        assert!(is_wireless_transport(
            "adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp"
        ));
        assert!(!is_wireless_transport("R5CX12ABCDE"));
        assert!(!is_wireless_transport(""));
    }

    /// `adb devices -l` output with a renamed mDNS transport, read the way
    /// `preferred_device_serial` reads it: the space in the name must not
    /// truncate or drop it.
    #[test]
    fn a_renamed_mdns_transport_is_still_a_connected_device() {
        let output = "List of devices attached\n\
                      adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp device product:m2quew model:SM_S947U1 device:m2q transport_id:1\n";
        assert_eq!(
            pick_preferred_serial(output).as_deref(),
            Some("adb-R5CX12ABCDE-62Sel0 (2)._adb-tls-connect._tcp")
        );
    }

    /// The same parse must not be fooled by `device:m2q` further along the
    /// line - that is why the pattern is non-greedy - and it prefers the
    /// ip:port of a phone that is also listed under its mDNS name.
    #[test]
    fn the_trailing_device_field_is_not_read_as_the_state() {
        let output = "192.168.0.4:40205      device product:m2quew model:SM_S947U1 device:m2q transport_id:5\n";
        assert_eq!(pick_preferred_serial(output).as_deref(), Some("192.168.0.4:40205"));
        let both = "List of devices attached\n\
                    adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp\tdevice\n\
                    192.168.0.4:40205\tdevice\n";
        assert_eq!(pick_preferred_serial(both).as_deref(), Some("192.168.0.4:40205"));
    }

    /// Two different phones on cables, neither with a wireless address: an
    /// empty host_port must not make them look like the same device. This is
    /// what the `!ip.is_empty()` guard in ip_matches is for.
    #[test]
    fn two_usb_phones_without_addresses_stay_separate() {
        let mut devices = vec![dev(1, "A", "", Some("SERA"))];
        upsert_device(&mut devices, "B".into(), String::new(), Some("SERB".into()));
        assert_eq!(devices.len(), 2);
    }

    // ---------- The phone app shipped inside the PC app ----------

    /// Trimmed from a real `dumpsys package` of an installed app: the package
    /// header, then its fields with the versionCode line.
    const DUMPSYS_INSTALLED: &str = "Activity Resolver Table:\n  Non-Data Actions:\n\
        Packages:\n  Package [com.mirryl.app] (5f1c2a7):\n    userId=10456\n\
            pkg=Package{8d03b14 com.mirryl.app}\n    versionCode=24 minSdk=26 targetSdk=35\n\
            versionName=1.0.11\n";

    #[test]
    fn an_installed_phone_app_reports_its_version_code() {
        assert_eq!(
            parse_installed_phone_app(DUMPSYS_INSTALLED, PACKAGE_NAME),
            InstalledPhoneApp::Version(24)
        );
    }

    #[test]
    fn newer_androids_say_the_phone_app_is_not_there() {
        assert_eq!(
            parse_installed_phone_app("Unable to find package: com.mirryl.app\n", PACKAGE_NAME),
            InstalledPhoneApp::Missing
        );
    }

    /// Older Androids print dumpsys's other sections and no package section;
    /// a package whose name only starts with ours is not ours.
    #[test]
    fn no_package_section_means_the_phone_app_is_not_there() {
        let older = "Dexopt state:\n\nCompiler stats:\n";
        assert_eq!(parse_installed_phone_app(older, PACKAGE_NAME), InstalledPhoneApp::Missing);
        let lookalike = "Packages:\n  Package [com.mirryl.app.debug] (1a2b):\n    versionCode=99 minSdk=26\n";
        assert_eq!(parse_installed_phone_app(lookalike, PACKAGE_NAME), InstalledPhoneApp::Missing);
    }

    /// What adb or run_process print instead of an answer must never read as
    /// "not installed": that would offer to install over an app that is there.
    #[test]
    fn adb_errors_say_nothing_about_the_phone_app() {
        for out in [
            "",
            "error: device offline",
            "error: device '192.168.0.4:40205' not found",
            "error: no devices/emulators found",
            "Error running adb: The system cannot find the file specified.",
            "Timed out after 40s waiting for adb -s x shell dumpsys package com.mirryl.app.",
        ] {
            assert_eq!(
                parse_installed_phone_app(out, PACKAGE_NAME),
                InstalledPhoneApp::Unknown,
                "{out:?}"
            );
        }
        let no_version = "Packages:\n  Package [com.mirryl.app] (5f1c2a7):\n    userId=10456\n";
        assert_eq!(parse_installed_phone_app(no_version, PACKAGE_NAME), InstalledPhoneApp::Unknown);
    }

    #[test]
    fn only_an_older_phone_app_is_offered_the_update() {
        use InstalledPhoneApp::*;
        assert_eq!(phone_app_state(Missing, Some(24)), PhoneAppState::Missing);
        assert_eq!(phone_app_state(Missing, None), PhoneAppState::Missing);
        assert_eq!(phone_app_state(Version(23), Some(24)), PhoneAppState::Outdated);
        assert_eq!(phone_app_state(Version(24), Some(24)), PhoneAppState::Ready);
        assert_eq!(phone_app_state(Version(25), Some(24)), PhoneAppState::Ready);
        assert_eq!(phone_app_state(Version(23), None), PhoneAppState::Ready);
        assert_eq!(phone_app_state(Unknown, Some(24)), PhoneAppState::Unknown);
    }

    #[test]
    fn the_version_file_holds_just_the_version_code() {
        assert_eq!(parse_phone_app_version_file("24\r\n"), Some(24));
        assert_eq!(parse_phone_app_version_file(" 25 "), Some(25));
        assert_eq!(parse_phone_app_version_file(""), None);
        assert_eq!(parse_phone_app_version_file("versionCode=24"), None);
    }

    /// A copy signed with another key is explained and never uninstalled from
    /// here; every explanation keeps Android's own words for whoever looks.
    #[test]
    fn install_failures_are_explained_with_androids_words_kept() {
        let signed = "Performing Streamed Install\nadb: failed to install Mirryl.apk: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Existing package com.mirryl.app signatures do not match newer version; ignoring!]";
        let text = explain_install_failure(signed);
        assert!(text.contains("Uninstall Mirryl on the phone"), "{text}");
        assert!(text.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE"), "{text}");
        let blocked = explain_install_failure("Failure [INSTALL_FAILED_USER_RESTRICTED: Install canceled by user]");
        assert!(blocked.contains("Auto Blocker"), "{blocked}");
        let other = explain_install_failure("Failure [INSTALL_FAILED_INVALID_APK]");
        assert!(other.starts_with("The Mirryl app could not be installed"), "{other}");
        assert!(other.contains("INSTALL_FAILED_INVALID_APK"), "{other}");
    }
}
