//! The Apps screen: the apps of one phone, and what can be done to them from
//! the PC - open, install, block, turn off, clear, uninstall, keep a copy.
//!
//! Everything goes over the adb connection the phone already has, as the shell
//! user. The list itself comes from a small program run on the phone for the
//! purpose (`apps-agent/`, embedded below) that prints each app's name, icon and
//! state; it is pushed to `/data/local/tmp` and is never installed. Nothing in
//! here leaves the PC and the phone.
//!
//! What cannot be undone is guarded twice: the page asks first, and this side
//! refuses on its own, whatever the page sends, to touch the apps the phone
//! needs to keep working (`is_protected`) or to remove a system app (those are
//! only ever turned off, which can be turned back on).

use super::{
    adb_path, log_activity, percent_encode_uri_segment, run_process, run_process_timeout,
    sanitize_for_filename, INSTALL_TIMEOUT, PACKAGE_NAME,
};
use serde::{Deserialize, Serialize};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::Manager;

/// The program that lists the apps on the phone. Built by
/// `apps-agent/build-apps-agent.ps1` from `apps-agent/src/`.
const APPS_AGENT: &[u8] = include_bytes!("../apps-agent/mirryl-apps.jar");
const APPS_AGENT_REMOTE: &str = "/data/local/tmp/mirryl-apps.jar";
const APPS_AGENT_CLASS: &str = "com.mirryl.apps.AppListMain";
/// Reading a few hundred icons takes some seconds on a phone (about 9 on an
/// emulator); this is the point past which the phone is not answering.
const LIST_TIMEOUT: Duration = Duration::from_secs(120);
/// Copying an app's APKs off the phone: big games are gigabytes over Wi-Fi.
const PULL_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// What the phone needs to keep working, or Mirryl needs to keep reaching it.
/// Only "Open" works on these; the default launcher joins them at run time.
const PROTECTED: &[&str] = &[
    PACKAGE_NAME,
    "android",
    "com.android.systemui",
    "com.android.settings",
    "com.android.shell",
    "com.android.phone",
    "com.android.vending",
    "com.android.packageinstaller",
    "com.google.android.packageinstaller",
    "com.google.android.gms",
    "com.google.android.gsf",
    "com.android.providers.settings",
    "com.samsung.android.packageinstaller",
];

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct PhoneApp {
    pub package: String,
    pub label: String,
    pub system: bool,
    pub enabled: bool,
    pub suspended: bool,
    /// "package/activity" of its icon in the app drawer, "" for none.
    #[serde(default)]
    pub activity: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub installed: u64,
    #[serde(default)]
    pub updated: u64,
    /// Where its APK files are on the phone: base first, then the splits.
    #[serde(default)]
    pub apks: Vec<String>,
    /// PNG, base64. Only apps with an icon in the app drawer have one.
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub protected: bool,
    /// Asks for the PIN before it opens (AppLock in the phone app).
    #[serde(default)]
    pub locked: bool,
}

#[derive(Debug, Serialize)]
pub struct AppListing {
    pub apps: Vec<PhoneApp>,
    pub home: String,
    /// The PIN protection: whether this phone's Mirryl app has it (27 on), and
    /// whether a PIN is set.
    pub lock: LockInfo,
}

#[derive(Debug, Default, Serialize)]
pub struct LockInfo {
    pub supported: bool,
    pub pin: bool,
}

/// What the Mirryl app on the phone answers about its PIN protection.
#[derive(Debug, Default, Deserialize, PartialEq)]
pub struct LockState {
    #[serde(default)]
    pub pin: bool,
    #[serde(default)]
    pub service: bool,
    #[serde(default)]
    pub locked: Vec<String>,
}

const LOCK_RECEIVER: &str = "com.mirryl.app/.LockConfigReceiver";
const LOCK_SERVICE: &str = "com.mirryl.app/com.mirryl.app.AppLockService";

/// The screens Android shows to install or uninstall an app. Protected along
/// with the first app, so nobody can uninstall Mirryl to get around the PIN.
const INSTALLERS: &[&str] = &[
    "com.android.packageinstaller",
    "com.google.android.packageinstaller",
    "com.samsung.android.packageinstaller",
];

/// Never behind a PIN: the phone itself, its screen, its calls (emergency
/// ones included), the launcher it goes back to, and Mirryl's own app.
const NEVER_PIN: &[&str] = &[
    PACKAGE_NAME,
    "android",
    "com.android.systemui",
    "com.android.shell",
    "com.android.phone",
    "com.android.providers.settings",
];

pub fn may_pin(package: &str, home: &str) -> bool {
    !NEVER_PIN.contains(&package) && (home.is_empty() || package != home)
}

/// The text between `data="` and the last `"` of `am broadcast`'s answer.
pub fn broadcast_data(output: &str) -> Option<String> {
    let start = output.find("data=\"")? + "data=\"".len();
    let end = output.rfind('"')?;
    (end > start).then(|| output[start..end].to_string())
}

fn lock_request(adb: &str, target: &str, extras: &[&str]) -> Option<String> {
    let mut args = vec!["am", "broadcast", "-n", LOCK_RECEIVER];
    args.extend_from_slice(extras);
    broadcast_data(&shell(adb, target, &args))
}

/// None when the phone's Mirryl app is older than the PIN protection, or absent.
fn lock_state(adb: &str, target: &str) -> Option<LockState> {
    lock_request(adb, target, &["--es", "op", "get"]).and_then(|data| serde_json::from_str(&data).ok())
}

/// Turns on the service that asks for the PIN, keeping any other accessibility
/// service the phone already has on.
fn ensure_lock_service(adb: &str, target: &str) {
    let current = shell(adb, target, &["settings", "get", "secure", "enabled_accessibility_services"]);
    let current = current.trim();
    let on = current
        .split(':')
        .any(|part| part.eq_ignore_ascii_case(LOCK_SERVICE) || part.eq_ignore_ascii_case("com.mirryl.app/.AppLockService"));
    if !on {
        let list = if current.is_empty() || current == "null" || adb_trouble(current) {
            LOCK_SERVICE.to_string()
        } else {
            format!("{current}:{LOCK_SERVICE}")
        };
        // Quoted for the phone's shell: the list is one argument.
        if !list.contains('\'') {
            let _ = shell(adb, target, &["settings", "put", "secure", "enabled_accessibility_services", &format!("'{list}'")]);
        }
    }
    let _ = shell(adb, target, &["settings", "put", "secure", "accessibility_enabled", "1"]);
}

pub fn is_pin(value: &str) -> bool {
    (4..=12).contains(&value.len()) && value.chars().all(|c| c.is_ascii_digit())
}

/// Sets the PIN that opens the protected apps of a phone. Mirryl on the phone
/// keeps only a salted hash of it.
#[tauri::command]
pub async fn set_lock_pin(target: String, alias: String, pin: String) -> AppActionResult {
    let target = target.trim().to_string();
    if target.is_empty() {
        return failed("Connect to the phone first.");
    }
    if !is_pin(&pin) {
        return failed("The PIN must be 4 to 12 digits.");
    }
    tauri::async_runtime::spawn_blocking(move || {
        let adb = adb_path();
        if lock_state(&adb, &target).is_none() {
            return failed(OLD_PHONE_APP);
        }
        let answer = lock_request(&adb, &target, &["--es", "op", "set_pin", "--es", "pin", &pin]).unwrap_or_default();
        log_activity(&format!("apps set PIN on {alias} ({target}): {}", if answer == "ok" { "ok" } else { &answer }));
        if answer == "ok" {
            done("The PIN is set. The protected apps of this phone open with it.")
        } else {
            failed(format!("The phone did not take the PIN.\n\nDetails: {answer}"))
        }
    })
    .await
    .unwrap_or_else(|e| failed(format!("Stopped: {e}")))
}

const OLD_PHONE_APP: &str = "The Mirryl app on this phone is older than the PIN protection. Press \"Reinstall the Mirryl app\" on this phone's page, then try again.";

/// Puts an app behind the PIN ("protect") or takes it out ("unprotect").
#[tauri::command]
pub async fn app_pin(target: String, alias: String, package: String, label: String, action: String) -> AppActionResult {
    tauri::async_runtime::spawn_blocking(move || {
        let target = target.trim();
        if target.is_empty() {
            return failed("Connect to the phone first.");
        }
        if !is_package_name(&package) {
            return failed("That is not an app name Android would use.");
        }
        let name = if label.trim().is_empty() { package.as_str() } else { label.trim() };
        let adb = adb_path();
        let Some(state) = lock_state(&adb, target) else {
            return failed(OLD_PHONE_APP);
        };
        let (op, message) = match action.as_str() {
            "protect" => {
                if !may_pin(&package, &home_package(&adb, target)) {
                    return failed(format!("{name} keeps the phone working (or Mirryl reaching it), so it is never put behind a PIN."));
                }
                if !state.pin {
                    return failed("Set a PIN for this phone first.");
                }
                ("lock", format!("{name} now asks for the PIN."))
            }
            "unprotect" => ("unlock", format!("{name} no longer asks for the PIN.")),
            _ => return failed("Unknown action."),
        };
        let answer = lock_request(&adb, target, &["--es", "op", op, "--es", "package", &package]).unwrap_or_default();
        log_activity(&format!("apps {action} {package} on {alias} ({target}): {answer}"));
        if answer != "ok" {
            return failed(format!("The phone did not take it.\n\nDetails: {answer}"));
        }
        let mut message = message;
        if op == "lock" {
            ensure_lock_service(&adb, target);
            // The first protected app brings the install and uninstall screens
            // with it: uninstalling Mirryl would switch the PIN off.
            let installed = shell(&adb, target, &["pm", "list", "packages"]);
            let mut added = false;
            for installer in INSTALLERS {
                let present = installed.lines().any(|line| line.trim() == format!("package:{installer}"));
                if present && !state.locked.iter().any(|p| p == installer) {
                    let _ = lock_request(&adb, target, &["--es", "op", "lock", "--es", "package", installer]);
                    added = true;
                }
            }
            if added {
                message.push_str(" Installing and uninstalling apps on the phone asks for it too, so nobody can remove Mirryl to get around it.");
            }
        }
        done(message)
    })
    .await
    .unwrap_or_else(|e| failed(format!("Stopped: {e}")))
}

#[derive(Debug, Serialize)]
pub struct AppActionResult {
    pub ok: bool,
    pub message: String,
}

fn done(message: impl Into<String>) -> AppActionResult {
    AppActionResult { ok: true, message: message.into() }
}

fn failed(message: impl Into<String>) -> AppActionResult {
    AppActionResult { ok: false, message: message.into() }
}

/// A package name as Android writes them. Anything else is refused before it
/// gets near the phone's shell, which reads the whole adb command line.
pub fn is_package_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|part| {
            !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// "package/activity" for `am start -n`, of that same package.
pub fn is_component_of(value: &str, package: &str) -> bool {
    let Some((pkg, class)) = value.split_once('/') else {
        return false;
    };
    pkg == package
        && !class.is_empty()
        && class.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'))
}

/// Without these the phone does not start, loses its screen or its calls, or
/// Mirryl loses the phone. When Android refuses to block an app, Block turns
/// it off instead - but never one of these.
const NEVER_TURNED_OFF: &[&str] = &[
    PACKAGE_NAME,
    "android",
    "com.android.systemui",
    "com.android.shell",
    "com.android.phone",
    "com.android.providers.settings",
    "com.android.packageinstaller",
    "com.google.android.packageinstaller",
    "com.samsung.android.packageinstaller",
];

pub fn may_turn_off_to_block(package: &str, home: &str) -> bool {
    !NEVER_TURNED_OFF.contains(&package) && (home.is_empty() || package != home)
}

pub fn is_protected(package: &str, home: &str) -> bool {
    PROTECTED.contains(&package) || (!home.is_empty() && package == home)
}

/// Whether `action` may be done to an app. Opening and the Play Store page
/// are always fine, and so is unblocking: it only gives back what blocking
/// took. Blocking works on every system app, protected or not: whoever runs
/// the phones decides what they keep open (Android itself still refuses a
/// few, such as Settings and the active launcher). Everything
/// else leaves the protected apps alone.
pub fn may_act(action: &str, protected: bool, system: bool) -> bool {
    match action {
        "open" | "play_store" | "unblock" => true,
        "block" => !protected || system,
        _ => !protected,
    }
}

/// The agent's output: a header line, one JSON line per app, and "END".
/// A list without its "END" was cut short and is not passed off as complete.
pub fn parse_listing(output: &str) -> Result<AppListing, String> {
    #[derive(Deserialize)]
    struct Header {
        #[serde(default)]
        home: String,
    }
    let mut home = String::new();
    let mut apps = Vec::new();
    let mut header_seen = false;
    let mut ended = false;
    for line in output.lines().map(str::trim) {
        if line == "END" {
            ended = true;
            break;
        }
        if let Some(error) = line.strip_prefix("ERROR ") {
            return Err(format!("The phone could not list its apps: {error}"));
        }
        if !line.starts_with('{') {
            continue;
        }
        if !header_seen {
            header_seen = true;
            if let Ok(header) = serde_json::from_str::<Header>(line) {
                home = header.home;
                continue;
            }
        }
        if let Ok(app) = serde_json::from_str::<PhoneApp>(line) {
            apps.push(app);
        }
    }
    if !ended {
        let tail: String = output.trim().chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!(
            "The phone stopped answering before the list was complete. Make sure it is still connected, then press Refresh.\n\nDetails: {tail}"
        ));
    }
    for app in &mut apps {
        app.protected = is_protected(&app.package, &home);
    }
    apps.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()).then(a.package.cmp(&b.package)));
    Ok(AppListing { apps, home, lock: LockInfo::default() })
}

/// The agent, written to a temporary file of its own so `adb push` has a path.
fn staged_agent() -> Result<PathBuf, String> {
    let path = env::temp_dir().join(format!("mirryl-apps-{}.jar", std::process::id()));
    std::fs::write(&path, APPS_AGENT).map_err(|e| format!("Could not prepare the app list: {e}"))?;
    Ok(path)
}

fn shell(adb: &str, target: &str, args: &[&str]) -> String {
    let mut full = vec!["-s", target, "shell"];
    full.extend_from_slice(args);
    run_process(adb, &full)
}

fn adb_trouble(output: &str) -> bool {
    output.contains("error:")
        || output.contains("device offline")
        || output.starts_with("Timed out")
        || output.starts_with("Error running")
}

#[tauri::command]
pub async fn list_apps(target: String) -> Result<AppListing, String> {
    let target = target.trim().to_string();
    if target.is_empty() {
        return Err("Connect to the phone first.".to_string());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let adb = adb_path();
        let local = staged_agent()?;
        let local_str = local.to_string_lossy().to_string();
        let pushed = run_process(&adb, &["-s", &target, "push", &local_str, APPS_AGENT_REMOTE]);
        let _ = std::fs::remove_file(&local);
        if !pushed.contains("pushed") {
            return Err(format!("Could not reach the phone. Make sure it is connected, then press Refresh.\n\nDetails: {}", pushed.trim()));
        }
        let mut listing = list_with_retries(&adb, &target)?;
        if let Some(state) = lock_state(&adb, &target) {
            for app in &mut listing.apps {
                app.locked = state.locked.iter().any(|p| p == &app.package);
            }
            listing.lock = LockInfo { supported: true, pin: state.pin };
            // Switched off on the phone (or never on): the protected apps
            // would open without asking, so it goes back on.
            if !state.locked.is_empty() && !state.service {
                ensure_lock_service(&adb, &target);
            }
        }
        Ok(listing)
    })
    .await
    .map_err(|e| format!("The app list stopped: {e}"))?
}

/// Apps whose icon (or whole entry) made the lister abort on a phone, kept
/// for as long as this app runs, so Refresh does not trip on them again.
fn skips() -> &'static std::sync::Mutex<std::collections::HashMap<String, Skips>> {
    static SKIPS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Skips>>> =
        std::sync::OnceLock::new();
    SKIPS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

#[derive(Clone, Default, Debug, PartialEq)]
pub struct Skips {
    pub no_icon: Vec<String>,
    pub bare: Vec<String>,
}

/// The app the lister was on when its output stops: the last "BEGIN p" with
/// no line for p after it. None when the output does not end that way.
pub fn unfinished_app(output: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for line in output.lines().map(str::trim) {
        if let Some(pkg) = line.strip_prefix("BEGIN ") {
            current = Some(pkg.trim().to_string());
        } else if line.starts_with('{') {
            if let Some(pkg) = current.as_deref() {
                if line.contains(&format!("\"package\":\"{pkg}\"")) {
                    current = None;
                }
            }
        }
    }
    current.filter(|pkg| is_package_name(pkg))
}

/// Where to go next after the lister aborted on `culprit`: first without its
/// icon, then without anything but its package name. None: nothing left to try.
pub fn skip_more(skips: &Skips, culprit: &str) -> Option<Skips> {
    let mut next = skips.clone();
    if skips.bare.iter().any(|p| p == culprit) {
        return None;
    }
    if skips.no_icon.iter().any(|p| p == culprit) {
        next.bare.push(culprit.to_string());
    } else {
        next.no_icon.push(culprit.to_string());
    }
    Some(next)
}

fn list_with_retries(adb: &str, target: &str) -> Result<AppListing, String> {
    let mut current = skips().lock().map(|m| m.get(target).cloned().unwrap_or_default()).unwrap_or_default();
    let mut last_error = String::new();
    for _ in 0..8 {
        let mut command = format!("CLASSPATH={APPS_AGENT_REMOTE} app_process / {APPS_AGENT_CLASS}");
        if !current.no_icon.is_empty() {
            command.push_str(&format!(" --noicon={}", current.no_icon.join(",")));
        }
        if !current.bare.is_empty() {
            command.push_str(&format!(" --bare={}", current.bare.join(",")));
        }
        let output = run_process_timeout(adb, &["-s", target, "exec-out", &command], LIST_TIMEOUT);
        match parse_listing(&output) {
            Ok(listing) => {
                if let Ok(mut map) = skips().lock() {
                    map.insert(target.to_string(), current);
                }
                return Ok(listing);
            }
            Err(error) => {
                last_error = error;
                let Some(next) = unfinished_app(&output).and_then(|culprit| {
                    log_activity(&format!("apps list on {target}: the phone stopped on {culprit}, listing again without it"));
                    skip_more(&current, &culprit)
                }) else {
                    break;
                };
                current = next;
            }
        }
    }
    Err(last_error)
}

/// The default launcher right now, asked again for every action rather than
/// trusted from the page: it is part of what `is_protected` keeps safe.
fn home_package(adb: &str, target: &str) -> String {
    let out = shell(
        adb,
        target,
        &["cmd", "package", "resolve-activity", "--brief", "-a", "android.intent.action.MAIN", "-c", "android.intent.category.HOME"],
    );
    out.lines()
        .rev()
        .map(str::trim)
        .find_map(|line| line.split_once('/').map(|(pkg, _)| pkg.to_string()))
        .filter(|pkg| is_package_name(pkg))
        .unwrap_or_default()
}

fn is_system_package(adb: &str, target: &str, package: &str) -> Option<bool> {
    let out = shell(adb, target, &["pm", "list", "packages", "-s"]);
    if adb_trouble(&out) {
        return None;
    }
    let wanted = format!("package:{package}");
    Some(out.lines().any(|line| line.trim() == wanted))
}

/// One action on one app. `action` is one of: open, force_stop, clear_data,
/// uninstall, disable, enable, block, unblock, play_store.
#[tauri::command]
pub async fn app_action(
    target: String,
    alias: String,
    package: String,
    label: String,
    action: String,
    activity: Option<String>,
) -> AppActionResult {
    tauri::async_runtime::spawn_blocking(move || {
        run_app_action(target.trim(), &alias, &package, &label, &action, activity.as_deref().unwrap_or(""))
    })
    .await
    .unwrap_or_else(|e| failed(format!("The action stopped: {e}")))
}

fn run_app_action(target: &str, alias: &str, package: &str, label: &str, action: &str, activity: &str) -> AppActionResult {
    if target.is_empty() {
        return failed("Connect to the phone first.");
    }
    if !is_package_name(package) {
        return failed("That is not an app name Android would use.");
    }
    let adb = adb_path();
    let name = if label.trim().is_empty() { package } else { label.trim() };

    let home = home_package(&adb, target);
    if !may_act(action, true, false) && is_protected(package, &home) {
        // Asked only for the protected ones: the system check is one more
        // round trip to the phone.
        let system = action == "block" && is_system_package(&adb, target, package) == Some(true);
        if !may_act(action, true, system) {
            return failed(format!(
                "{name} is something the phone needs to keep working (or that Mirryl needs to reach it), so from here it can only be opened{}.",
                if system { " or blocked" } else { "" }
            ));
        }
    }

    let (output, ok, message) = match action {
        "open" => {
            let out = if is_component_of(activity, package) {
                shell(&adb, target, &["am", "start", "-n", activity])
            } else {
                shell(&adb, target, &["monkey", "-p", package, "-c", "android.intent.category.LAUNCHER", "1"])
            };
            let ok = !adb_trouble(&out) && !out.contains("Error") && !out.contains("No activities found");
            (out, ok, format!("{name} was opened on the phone."))
        }
        "force_stop" => {
            let out = shell(&adb, target, &["am", "force-stop", package]);
            let ok = !adb_trouble(&out) && out.trim().is_empty();
            (out, ok, format!("{name} was stopped. It starts again the next time it is opened."))
        }
        "clear_data" => {
            let out = shell(&adb, target, &["pm", "clear", "--user", "0", package]);
            let ok = out.contains("Success");
            (out, ok, format!("The data of {name} was deleted. It starts as if it had just been installed."))
        }
        "uninstall" => match is_system_package(&adb, target, package) {
            None => return failed("Could not reach the phone. Make sure it is connected, then try again."),
            Some(true) => {
                return failed(format!(
                    "{name} came with the phone, so it is not removed from here. Use \"Turn off\" instead: it disappears from the phone and can be turned back on."
                ))
            }
            Some(false) => {
                let out = shell(&adb, target, &["pm", "uninstall", "--user", "0", package]);
                let ok = out.contains("Success");
                (out, ok, format!("{name} was uninstalled from the phone."))
            }
        },
        "disable" => {
            let out = shell(&adb, target, &["pm", "disable-user", "--user", "0", package]);
            let ok = out.contains("disabled-user");
            (out, ok, format!("{name} was turned off. It no longer shows on the phone; \"Turn on\" brings it back."))
        }
        "enable" => {
            let out = shell(&adb, target, &["pm", "enable", "--user", "0", package]);
            let ok = out.contains("new state: enabled");
            (out, ok, format!("{name} was turned on again."))
        }
        "block" => {
            let out = shell(&adb, target, &["pm", "suspend", "--user", "0", package]);
            if out.contains("suspended state: true") {
                (out, true, format!("{name} was blocked: it stays on the phone, greyed out, and does not open until it is unblocked from here."))
            } else if out.contains("suspended state: false")
                && may_turn_off_to_block(package, &home)
                && is_system_package(&adb, target, package) == Some(true)
            {
                // Android keeps a few system apps from being blocked (Settings,
                // the Play Store). Turned off, they are just as unusable, and
                // Unblock brings them back the same way.
                let off = shell(&adb, target, &["pm", "disable-user", "--user", "0", package]);
                let ok = off.contains("disabled-user");
                (format!("{} | {}", out.trim(), off.trim()), ok, format!(
                    "Android does not let {name} be blocked, so it was turned off instead: it is gone from the phone until it is unblocked from here."
                ))
            } else {
                (out, false, String::new())
            }
        }
        "unblock" => {
            // Undoes either way Block may have taken: suspended, or turned off.
            let out = shell(&adb, target, &["pm", "unsuspend", "--user", "0", package]);
            let mut ok = out.contains("suspended state: false");
            let disabled = shell(&adb, target, &["pm", "list", "packages", "-d"]);
            let mut output = out.trim().to_string();
            if disabled.lines().any(|line| line.trim() == format!("package:{package}")) {
                let on = shell(&adb, target, &["pm", "enable", "--user", "0", package]);
                ok = on.contains("new state: enabled");
                output = format!("{output} | {}", on.trim());
            }
            (output, ok, format!("{name} was unblocked."))
        }
        "play_store" => {
            let uri = format!("'market://details?id={package}'");
            let out = shell(&adb, target, &["am", "start", "-a", "android.intent.action.VIEW", "-d", &uri]);
            let ok = !adb_trouble(&out) && !out.contains("Error");
            (out, ok, format!("The Play Store page of {name} was opened on the phone."))
        }
        _ => return failed("Unknown action."),
    };
    log_activity(&format!(
        "apps {action} {package} on {alias} ({target}): {}",
        if ok { "ok".to_string() } else { output.trim().replace('\n', " | ") }
    ));
    if ok {
        done(message)
    } else {
        failed(explain_action_failure(action, name, &output))
    }
}

fn explain_action_failure(action: &str, name: &str, output: &str) -> String {
    let out = output.trim();
    let reason = if adb_trouble(out) {
        "The phone did not answer. Make sure it is still connected, then try again.".to_string()
    } else if out.contains("SecurityException") || out.contains("not allowed") || out.contains("Cannot") || out.contains("cannot") {
        format!("Android does not let a computer do that to {name}.")
    } else {
        let what = match action {
            "open" => "open",
            "force_stop" => "stop",
            "clear_data" => "delete the data of",
            "uninstall" => "uninstall",
            "disable" => "turn off",
            "enable" => "turn on",
            "block" => "block",
            "unblock" => "unblock",
            _ => "do that to",
        };
        format!("The phone could not {what} {name}.")
    };
    if out.is_empty() {
        reason
    } else {
        format!("{reason}\n\nDetails from Android: {out}")
    }
}

/// Puts the Mirryl app back on a phone that is connected but lost it (it was
/// uninstalled on the phone): installs the copy this build carries, then gives
/// it what pairing gives it - the permission to turn Wireless debugging back
/// on, the battery exemption and the PC's signature - over the connection the
/// phone already has, with no pairing again.
#[tauri::command]
pub async fn reinstall_phone_app(app: tauri::AppHandle, target: String, alias: String) -> AppActionResult {
    let target = target.trim().to_string();
    if target.is_empty() {
        return failed("Connect to the phone first.");
    }
    let installed = super::install_phone_app(target.clone()).await;
    if !installed.ok {
        log_activity(&format!("apps reinstall phone app on {alias} ({target}): failed"));
        return failed(installed.message);
    }
    tauri::async_runtime::spawn_blocking(move || {
        let adb = adb_path();
        let grant = shell(&adb, &target, &["pm", "grant", PACKAGE_NAME, "android.permission.WRITE_SECURE_SETTINGS"]);
        let _ = shell(&adb, &target, &["dumpsys", "deviceidle", "whitelist", &format!("+{PACKAGE_NAME}")]);
        let notice = super::matriculation_notice(super::matriculation::matriculate(&app, &adb, &target));
        log_activity(&format!(
            "apps reinstall phone app on {alias} ({target}): installed, grant {}",
            if grant.trim().is_empty() { "ok".to_string() } else { grant.trim().replace('\n', " | ") }
        ));
        if !grant.trim().is_empty() {
            return failed(format!(
                "The Mirryl app is back on the phone, but Android did not give it the permission to turn Wireless debugging back on.\n\nDetails from Android: {}",
                grant.trim()
            ));
        }
        let mut message = "The Mirryl app is back on the phone, with its permission: it turns Wireless debugging back on by itself again.".to_string();
        if !notice.is_empty() {
            message.push_str("\n\n");
            message.push_str(&notice);
        }
        done(message)
    })
    .await
    .unwrap_or_else(|e| failed(format!("The installation stopped: {e}")))
}

/// Opens the Play Store on the phone, searching for `query`. Installing from
/// there is the phone owner's tap: Android has no way to do it from a computer.
#[tauri::command]
pub async fn play_store_search(target: String, query: String) -> AppActionResult {
    let target = target.trim().to_string();
    let query = query.trim().to_string();
    if target.is_empty() {
        return failed("Connect to the phone first.");
    }
    if query.is_empty() {
        return failed("Type what to look for first.");
    }
    tauri::async_runtime::spawn_blocking(move || {
        // Percent-encoded, the query is only letters, digits and - _ . %, so
        // the phone's shell has nothing in it to read as its own.
        let uri = format!("'market://search?q={}'", percent_encode_uri_segment(&query));
        let out = shell(&adb_path(), &target, &["am", "start", "-a", "android.intent.action.VIEW", "-d", &uri]);
        if adb_trouble(&out) || out.contains("Error") {
            failed(format!("The Play Store did not open on the phone.\n\nDetails from Android: {}", out.trim()))
        } else {
            done("The Play Store is open on the phone with that search. Installing is a tap on the phone.")
        }
    })
    .await
    .unwrap_or_else(|e| failed(format!("The search stopped: {e}")))
}

fn explain_apk_install_failure(file: &str, output: &str) -> String {
    let reason = if output.contains("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
        "The phone already has this app signed by someone else, and Android won't replace it. Uninstall it first, then install this one."
    } else if output.contains("INSTALL_FAILED_VERSION_DOWNGRADE") {
        "The phone already has a newer version of this app."
    } else if output.contains("INSTALL_FAILED_INSUFFICIENT_STORAGE") {
        "The phone doesn't have enough free space."
    } else if output.contains("INSTALL_FAILED_OLDER_SDK") {
        "The app needs a newer Android than the phone has."
    } else if output.contains("INSTALL_FAILED_NO_MATCHING_ABIS") {
        "The app was made for another kind of processor than the phone's."
    } else if output.contains("INSTALL_PARSE_FAILED") || output.contains("INSTALL_FAILED_INVALID_APK") {
        "The file is not a complete Android app. Some apps come in several files (.apks, .xapk) and are not installed this way."
    } else if output.contains("INSTALL_FAILED_USER_RESTRICTED")
        || output.contains("INSTALL_FAILED_ABORTED")
        || output.contains("INSTALL_FAILED_VERIFICATION_FAILURE")
        || output.to_lowercase().contains("user rejected")
    {
        "The phone refused the installation. If it asked for confirmation, accept it. On Samsung phones, Auto Blocker (Settings > Security and privacy) can block installs from a computer."
    } else if output.starts_with("Timed out") {
        "The phone didn't answer in time. Make sure it is awake and unlocked, and look for a confirmation on its screen."
    } else {
        "The app could not be installed."
    };
    format!("{file}: {reason}\n\nDetails from Android: {}", output.trim())
}

/// Installs APK files from this PC on the phone, one after the other, keeping
/// the data of an older copy of the same app (`-r`).
#[tauri::command]
pub async fn install_apks(target: String, alias: String, paths: Vec<String>) -> Vec<AppActionResult> {
    let target = target.trim().to_string();
    tauri::async_runtime::spawn_blocking(move || {
        let adb = adb_path();
        paths
            .iter()
            .map(|path| {
                let file = Path::new(path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.clone());
                if target.is_empty() {
                    return failed("Connect to the phone first.");
                }
                if !path.to_lowercase().ends_with(".apk") || !Path::new(path).is_file() {
                    return failed(format!("{file}: only .apk files can be installed from here."));
                }
                let out = run_process_timeout(&adb, &["-s", &target, "install", "-r", path], INSTALL_TIMEOUT);
                let ok = out.lines().any(|line| line.trim() == "Success");
                log_activity(&format!(
                    "apps install {file} on {alias} ({target}): {}",
                    if ok { "ok".to_string() } else { out.trim().replace('\n', " | ") }
                ));
                if ok {
                    done(format!("{file} was installed on the phone."))
                } else {
                    failed(explain_apk_install_failure(&file, &out))
                }
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// The device's own folder, the one the other device tools already use:
/// Documents\Mirryl\<device>.
fn device_folder(alias: &str) -> Option<PathBuf> {
    let base = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).map(PathBuf::from)?;
    Some(base.join("Documents").join("Mirryl").join(sanitize_for_filename(alias)))
}

/// A path on the phone as the agent reported it: absolute, no quoting needed.
fn is_remote_apk_path(path: &str) -> bool {
    path.starts_with('/')
        && path.ends_with(".apk")
        && !path.contains("..")
        && path.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '=' | '@' | '+'))
}

/// Copies an app's APK files from the phone to Documents\Mirryl\<device>\APKs
/// and shows them in Explorer. One file is saved as "<name> <version>.apk";
/// an app split in several goes to a folder of that name with all of them.
#[tauri::command]
pub async fn backup_apk(
    target: String,
    alias: String,
    package: String,
    label: String,
    version: String,
    apks: Vec<String>,
) -> AppActionResult {
    tauri::async_runtime::spawn_blocking(move || {
        let target = target.trim();
        if target.is_empty() {
            return failed("Connect to the phone first.");
        }
        if !is_package_name(&package) || apks.is_empty() || !apks.iter().all(|p| is_remote_apk_path(p)) {
            return failed("Mirryl could not find where this app's files are on the phone. Press Refresh and try again.");
        }
        let Some(folder) = device_folder(&alias).map(|dir| dir.join("APKs")) else {
            return failed("Could not find the Documents folder.");
        };
        let name = sanitize_for_filename(&format!(
            "{} {}",
            if label.trim().is_empty() { &package } else { label.trim() },
            version.trim()
        ));
        let destination = if apks.len() == 1 { folder.clone() } else { folder.join(&name) };
        if let Err(e) = std::fs::create_dir_all(&destination) {
            return failed(format!("Could not create {}: {e}", destination.display()));
        }
        let adb = adb_path();
        let destination_str = destination.to_string_lossy().to_string();
        for remote in &apks {
            let out = run_process_timeout(&adb, &["-s", target, "pull", remote, &destination_str], PULL_TIMEOUT);
            if !out.contains("pulled") {
                log_activity(&format!("apps backup {package} on {alias} ({target}): {}", out.trim().replace('\n', " | ")));
                return failed(format!("The copy did not finish.\n\nDetails: {}", out.trim()));
            }
        }
        let saved = if apks.len() == 1 {
            let pulled_name = Path::new(&apks[0]).file_name().map(|n| n.to_os_string()).unwrap_or_default();
            let pulled = destination.join(pulled_name);
            let renamed = folder.join(format!("{name}.apk"));
            let _ = std::fs::remove_file(&renamed);
            match std::fs::rename(&pulled, &renamed) {
                Ok(()) => renamed,
                Err(_) => pulled,
            }
        } else {
            destination
        };
        log_activity(&format!("apps backup {package} on {alias} ({target}): {}", saved.display()));
        reveal(&saved);
        done(format!("Saved to {}", saved.display()))
    })
    .await
    .unwrap_or_else(|e| failed(format!("The copy stopped: {e}")))
}

#[cfg(windows)]
fn reveal(path: &Path) {
    let mut select = std::ffi::OsString::from("/select,");
    select.push(path.as_os_str());
    let _ = Command::new("explorer.exe").arg(select).spawn();
}

#[cfg(not(windows))]
fn reveal(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = Command::new("xdg-open").arg(dir).spawn();
    }
}

/// The file picker for "Install an APK…": .apk files only, several at once.
/// Returns the chosen paths, none for cancel.
#[cfg(windows)]
#[tauri::command]
pub async fn pick_apks() -> Vec<String> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$dialog = New-Object System.Windows.Forms.OpenFileDialog
$dialog.Title = "Choose the apps to install on the phone"
$dialog.Filter = "Android apps (*.apk)|*.apk"
$dialog.Multiselect = $true
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
    $dialog.FileNames | ForEach-Object { Write-Output $_ }
}
"#;
    let picked = tauri::async_runtime::spawn_blocking(move || super::run_powershell_sta(script))
        .await
        .unwrap_or_default();
    picked
        .lines()
        .map(str::trim)
        .filter(|line| line.to_lowercase().ends_with(".apk") && Path::new(line).is_file())
        .map(str::to_string)
        .collect()
}

/// Linux: zenity's file chooser (a dependency of the package), .apk files
/// only, several at once. Same contract as on Windows.
#[cfg(not(windows))]
#[tauri::command]
pub async fn pick_apks() -> Vec<String> {
    let picked = tauri::async_runtime::spawn_blocking(|| {
        Command::new("zenity")
            .args([
                "--file-selection",
                "--multiple",
                "--separator=\n",
                "--title=Choose the apps to install on the phone",
                "--file-filter=Android apps (*.apk) | *.apk *.APK",
            ])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    picked
        .lines()
        .map(str::trim)
        .filter(|line| line.to_lowercase().ends_with(".apk") && Path::new(line).is_file())
        .map(str::to_string)
        .collect()
}


// ---------- Several phones: inventory, profiles, the watcher's alerts ----------

/// Saves the apps inventory the page put together (CSV) in
/// Documents\Mirryl\Inventory and shows it in Explorer.
#[tauri::command]
pub async fn save_inventory(file_name: String, csv: String) -> AppActionResult {
    let ok_name = file_name.len() <= 100
        && file_name.to_lowercase().ends_with(".csv")
        && file_name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '));
    if !ok_name || file_name.starts_with('.') {
        return failed("That file name is not allowed.");
    }
    let Some(base) = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME")).map(PathBuf::from) else {
        return failed("Could not find the Documents folder.");
    };
    let folder = base.join("Documents").join("Mirryl").join("Inventory");
    if let Err(e) = std::fs::create_dir_all(&folder) {
        return failed(format!("Could not create {}: {e}", folder.display()));
    }
    let path = folder.join(&file_name);
    // The BOM is what makes Excel read the names as UTF-8.
    match std::fs::write(&path, format!("\u{feff}{csv}")) {
        Ok(()) => {
            log_activity(&format!("apps inventory saved to {}", path.display()));
            reveal(&path);
            done(format!("Saved to {}", path.display()))
        }
        Err(e) => failed(format!("Could not write {}: {e}", path.display())),
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ProfileApp {
    pub package: String,
    #[serde(default)]
    pub label: String,
}

/// A set of apps to block and apps to protect with the PIN, saved by name and
/// applied to one phone or several.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub block: Vec<ProfileApp>,
    #[serde(default)]
    pub pin: Vec<ProfileApp>,
}

fn profiles_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("profiles.json"))
}

pub fn valid_profile(profile: &Profile) -> bool {
    let name = profile.name.trim();
    !name.is_empty()
        && name.chars().count() <= 60
        && profile.block.iter().chain(profile.pin.iter()).all(|a| is_package_name(&a.package))
}

#[tauri::command]
pub fn get_profiles(app: tauri::AppHandle) -> Vec<Profile> {
    profiles_path(&app)
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

#[tauri::command]
pub fn save_profiles(app: tauri::AppHandle, profiles: Vec<Profile>) -> Result<(), String> {
    if !profiles.iter().all(valid_profile) {
        return Err("A profile needs a name of up to 60 characters, and only app names Android would use.".to_string());
    }
    let path = profiles_path(&app)?;
    let text = serde_json::to_string_pretty(&profiles).map_err(|e| e.to_string())?;
    // Written next to it first and swapped in, so a crash never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub ok: bool,
    /// No PIN on this phone yet and none was given: ask for one and run again.
    pub needs_pin: bool,
    pub message: String,
    pub lines: Vec<String>,
}

/// Applies a profile to one phone: blocks its "block" apps and protects its
/// "pin" apps. With `exact`, also unblocks and takes the PIN off what the
/// profile does not name, so the phone ends up as the profile says.
#[tauri::command]
pub async fn apply_profile(target: String, alias: String, profile: Profile, exact: bool, pin: Option<String>) -> ApplyResult {
    tauri::async_runtime::spawn_blocking(move || run_apply_profile(target.trim(), &alias, &profile, exact, pin.as_deref()))
        .await
        .unwrap_or_else(|e| ApplyResult { ok: false, needs_pin: false, message: format!("Stopped: {e}"), lines: Vec::new() })
}

fn run_apply_profile(target: &str, alias: &str, profile: &Profile, exact: bool, pin: Option<&str>) -> ApplyResult {
    let stop = |message: String| ApplyResult { ok: false, needs_pin: false, message, lines: Vec::new() };
    if target.is_empty() {
        return stop("Not connected.".to_string());
    }
    if !valid_profile(profile) {
        return stop("The profile is not valid.".to_string());
    }
    let adb = adb_path();
    let listing = match list_with_retries(&adb, target) {
        Ok(listing) => listing,
        Err(e) => return stop(e),
    };
    let find = |pkg: &str| listing.apps.iter().find(|a| a.package == pkg);
    let mut lines = Vec::new();
    let mut changed = 0usize;

    let wants_pin = !profile.pin.is_empty();
    let state = if wants_pin || exact { lock_state(&adb, target) } else { None };
    let mut pin_ready = false;
    if wants_pin {
        match &state {
            None => lines.push("The Mirryl app on this phone is older than the PIN protection: the PIN apps were skipped. Press \"Reinstall the Mirryl app\" on its page.".to_string()),
            Some(st) if st.pin => pin_ready = true,
            Some(_) => match pin {
                Some(p) if is_pin(p) => {
                    let answer = lock_request(&adb, target, &["--es", "op", "set_pin", "--es", "pin", p]).unwrap_or_default();
                    if answer == "ok" {
                        log_activity(&format!("apps set PIN on {alias} ({target}) for a profile: ok"));
                        pin_ready = true;
                    } else {
                        lines.push(format!("The phone did not take the PIN: {answer}"));
                    }
                }
                _ => {
                    return ApplyResult { ok: false, needs_pin: true, message: "This phone has no PIN yet.".to_string(), lines };
                }
            },
        }
    }

    for wanted in &profile.block {
        let name = if wanted.label.is_empty() { &wanted.package } else { &wanted.label };
        match find(&wanted.package) {
            None => lines.push(format!("{name}: not on this phone")),
            Some(app) if app.suspended || !app.enabled => {}
            Some(app) => {
                let r = run_app_action(target, alias, &app.package, &app.label, "block", "");
                if r.ok { changed += 1 } else { lines.push(format!("{}: {}", app.label, first_line(&r.message))) }
            }
        }
    }
    if pin_ready {
        for wanted in &profile.pin {
            let name = if wanted.label.is_empty() { &wanted.package } else { &wanted.label };
            match find(&wanted.package) {
                None => lines.push(format!("{name}: not on this phone")),
                Some(app) if state.as_ref().is_some_and(|st| st.locked.contains(&app.package)) => {}
                Some(app) => {
                    let r = tauri::async_runtime::block_on(app_pin(target.to_string(), alias.to_string(), app.package.clone(), app.label.clone(), "protect".to_string()));
                    if r.ok { changed += 1 } else { lines.push(format!("{}: {}", app.label, first_line(&r.message))) }
                }
            }
        }
    }
    if exact {
        let in_block = |pkg: &str| profile.block.iter().any(|a| a.package == pkg);
        let in_pin = |pkg: &str| profile.pin.iter().any(|a| a.package == pkg);
        for app in listing.apps.iter().filter(|a| a.suspended && !in_block(&a.package)) {
            let r = run_app_action(target, alias, &app.package, &app.label, "unblock", "");
            if r.ok { changed += 1 } else { lines.push(format!("{}: {}", app.label, first_line(&r.message))) }
        }
        if let Some(st) = &state {
            for pkg in st.locked.iter().filter(|p| !in_pin(p)) {
                // The install screens stay protected while any app is.
                if wants_pin && INSTALLERS.contains(&pkg.as_str()) {
                    continue;
                }
                let label = find(pkg).map(|a| a.label.clone()).unwrap_or_else(|| pkg.clone());
                let r = tauri::async_runtime::block_on(app_pin(target.to_string(), alias.to_string(), pkg.clone(), label.clone(), "unprotect".to_string()));
                if r.ok { changed += 1 } else { lines.push(format!("{label}: {}", first_line(&r.message))) }
            }
        }
    }
    log_activity(&format!("apps profile \"{}\" applied to {alias} ({target}): {changed} changes, {} notes", profile.name, lines.len()));
    ApplyResult {
        ok: true,
        needs_pin: false,
        message: if changed == 0 { "Already as the profile says.".to_string() } else { format!("{changed} changes.") },
        lines,
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

/// The watcher's notes that it found the PIN protection off on a phone and
/// turned it back on: the lines of watcher.log that say so.
#[tauri::command]
pub fn pin_alerts() -> Vec<String> {
    let Some(log) = super::local_data_dir().map(|dir| dir.join("watcher.log")) else {
        return Vec::new();
    };
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(PIN_ALERT_MARK))
        .map(str::to_string)
        .collect()
}

/// What watcher.ps1 writes in each of those lines (Test-PinProtection).
pub const PIN_ALERT_MARK: &str = "PIN protection was off";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        assert!(is_package_name("com.whatsapp"));
        assert!(is_package_name("com.google.android.apps.nexuslauncher"));
        assert!(is_package_name("android"));
        assert!(!is_package_name(""));
        assert!(!is_package_name("com.x;reboot"));
        assert!(!is_package_name("com..x"));
        assert!(!is_package_name("com.x y"));
        assert!(!is_package_name("com.x'"));
    }

    #[test]
    fn components() {
        assert!(is_component_of("com.a/com.a.Main", "com.a"));
        assert!(is_component_of("com.a/.Main$Inner", "com.a"));
        assert!(!is_component_of("com.b/com.a.Main", "com.a"));
        assert!(!is_component_of("com.a/", "com.a"));
        assert!(!is_component_of("com.a/x;reboot", "com.a"));
    }

    #[test]
    fn protected_packages() {
        assert!(is_protected(PACKAGE_NAME, ""));
        assert!(is_protected("com.android.settings", ""));
        assert!(is_protected("com.sec.android.app.launcher", "com.sec.android.app.launcher"));
        assert!(!is_protected("com.whatsapp", "com.sec.android.app.launcher"));
    }

    #[test]
    fn what_may_be_done() {
        for action in ["open", "play_store", "unblock"] {
            assert!(may_act(action, true, false), "{action} is always allowed");
        }
        assert!(may_act("block", true, true), "a protected system app can be blocked");
        assert!(!may_act("block", true, false), "the Mirryl phone app cannot");
        assert!(may_act("block", false, false));
        for action in ["force_stop", "clear_data", "uninstall", "disable", "enable"] {
            assert!(!may_act(action, true, true), "{action} leaves a protected app alone");
            assert!(may_act(action, false, true));
        }
    }

    #[test]
    fn turned_off_instead_of_blocked() {
        let home = "com.sec.android.app.launcher";
        assert!(may_turn_off_to_block("com.android.settings", home));
        assert!(may_turn_off_to_block("com.android.vending", home));
        assert!(!may_turn_off_to_block("com.android.systemui", home));
        assert!(!may_turn_off_to_block(home, home), "never the launcher");
        assert!(!may_turn_off_to_block(PACKAGE_NAME, home));
    }

    #[test]
    fn pin_rules() {
        assert!(is_pin("4321"));
        assert!(is_pin("123456789012"));
        assert!(!is_pin("123"));
        assert!(!is_pin("12ab"));
        assert!(!is_pin("1234; reboot"));
        let home = "com.sec.android.app.launcher";
        assert!(may_pin("com.android.settings", home));
        assert!(may_pin("com.google.android.packageinstaller", home));
        assert!(!may_pin(home, home));
        assert!(!may_pin(PACKAGE_NAME, home));
        assert!(!may_pin("com.android.systemui", home));
    }

    #[test]
    fn broadcast_answers() {
        let out = "Broadcasting: Intent { cmp=com.mirryl.app/.LockConfigReceiver (has extras) }\nBroadcast completed: result=0, data=\"{\"version\":1,\"pin\":true,\"service\":false,\"locked\":[\"com.android.settings\"]}\"";
        let data = broadcast_data(out).unwrap();
        let state: LockState = serde_json::from_str(&data).unwrap();
        assert!(state.pin && !state.service);
        assert_eq!(state.locked, vec!["com.android.settings".to_string()]);
        assert_eq!(broadcast_data("Broadcast completed: result=0"), None);
        assert_eq!(broadcast_data("Broadcast completed: result=0, data=\"ok\"").as_deref(), Some("ok"));
    }

    #[test]
    fn finds_where_the_list_stopped() {
        let out = "{\"home\":\"l\"}\nBEGIN com.a\n{\"package\":\"com.a\",\"label\":\"A\",\"system\":false,\"enabled\":true,\"suspended\":false}\nBEGIN com.b\nAssertion failed: src == nullptr\nAborted";
        assert_eq!(unfinished_app(out).as_deref(), Some("com.b"));
        let done = "BEGIN com.a\n{\"package\":\"com.a\"}\nEND";
        assert_eq!(unfinished_app(done), None);
        let s1 = skip_more(&Skips::default(), "com.b").unwrap();
        assert_eq!(s1.no_icon, vec!["com.b".to_string()]);
        let s2 = skip_more(&s1, "com.b").unwrap();
        assert_eq!(s2.bare, vec!["com.b".to_string()]);
        assert_eq!(skip_more(&s2, "com.b"), None);
    }

    #[test]
    fn profiles_are_checked() {
        let good = Profile { name: "Empleado".into(), block: vec![ProfileApp { package: "com.whatsapp".into(), label: "WhatsApp".into() }], pin: vec![] };
        assert!(valid_profile(&good));
        let mut bad = good.clone();
        bad.name = "  ".into();
        assert!(!valid_profile(&bad));
        let mut bad = good.clone();
        bad.pin.push(ProfileApp { package: "x;reboot".into(), label: String::new() });
        assert!(!valid_profile(&bad));
        let text = serde_json::to_string(&vec![good.clone()]).unwrap();
        let back: Vec<Profile> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, vec![good]);
    }

    #[test]
    fn remote_paths() {
        assert!(is_remote_apk_path("/data/app/~~eWha==/com.mirryl.app-ts-Wi_q==/base.apk"));
        assert!(is_remote_apk_path("/system/priv-app/Foo@1/Foo.apk"));
        assert!(!is_remote_apk_path("data/app/base.apk"));
        assert!(!is_remote_apk_path("/data/app/x; reboot.apk"));
        assert!(!is_remote_apk_path("/data/../x.apk"));
        assert!(!is_remote_apk_path("/data/app/base.txt"));
    }

    #[test]
    fn listing_needs_its_end() {
        let full = "{\"home\":\"com.l\",\"sdk\":36}\n\
            {\"package\":\"com.b\",\"label\":\"beta\",\"system\":false,\"enabled\":true,\"suspended\":false,\"activity\":\"com.b/.M\",\"apks\":[\"/data/app/b/base.apk\"],\"icon\":\"\"}\n\
            {\"package\":\"com.l\",\"label\":\"Alpha\",\"system\":true,\"enabled\":true,\"suspended\":true}\n\
            END\n";
        let listing = parse_listing(full).unwrap();
        assert_eq!(listing.home, "com.l");
        assert_eq!(listing.apps.len(), 2);
        assert_eq!(listing.apps[0].label, "Alpha");
        assert!(listing.apps[0].protected, "the launcher is protected");
        assert!(listing.apps[0].suspended);
        assert!(!listing.apps[1].protected);
        assert!(parse_listing(&full.replace("END\n", "")).is_err());
        assert!(parse_listing("ERROR java.lang.Boom").is_err());
    }
}
