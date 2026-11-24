use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct SessionLog {
    path: PathBuf,
    serial: String,
    started: Instant,
    file: Mutex<File>,
}

/// How much the session logs of ONE phone may take (the files whose name
/// carries its serial), not the whole folder.
///
/// 5 MB per phone: ten phones, 50 MB; three, 15 MB. A cap on the whole folder
/// would delete some phones' sessions to keep others'. Without a cap,
/// these files grow for as long as the app is installed, and each one carries
/// the phone's identifier in its name and on every line - so this is not only
/// disk, it is personal data with no expiry. The cap is what lets the privacy
/// policy state a retention instead of keeping quiet about it.
///
/// "Phone" here is the serial the session was opened with: the same device
/// over a cable (hardware serial) and over Wi-Fi (ip:port) counts as two.
const MAX_LOG_BYTES_PER_PHONE: u64 = 5 * 1024 * 1024;

/// The serial a log was named with (`mirryl-<ms>-<pid>-<serial>.jsonl`),
/// or "" for a name that does not follow that pattern (logs from earlier
/// versions, tests): those form their own group with the same cap.
fn serial_of_log(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut parts = stem.splitn(4, '-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("mirryl"), Some(_ms), Some(_pid), Some(serial)) => serial.to_owned(),
        _ => String::new(),
    }
}

/// Deletes each phone's oldest logs until its own fall under the cap.
///
/// Three decisions that are not details:
///
/// - **WHOLE files are deleted**, never trimmed inside. A log whose head is
///   cut off loses the `ready` event with the size, the engine version and
///   the session start, and without that the figures left cannot be
///   interpreted. One whole file less is one session less; half a file is a
///   useless session.
/// - **The oldest are counted and deleted first**, by modification time,
///   which is when the session ended.
/// - **Every error is ignored silently**, and that is deliberate: on Windows,
///   deleting the log ANOTHER engine has open fails, and that is exactly what
///   must happen. A running session is never touched. It fails because
///   `open_log` opens it WITHOUT delete sharing; with Rust's default
///   `OpenOptions` the delete would work, and the live log would vanish while
///   the engine keeps writing into nothing. It is not retried
///   either: the next start tries again, and meanwhile that file still counts
///   toward the cap, so there is no loop and no surprise.
///
/// The cap is checked when a session STARTS, not during it. A very long
/// session can go over the cap while it runs; it is trimmed at the next
/// start. That is what the policy has to say, and it does.
fn sweep_old_logs(directory: &Path, keep_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut logs: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            // With no readable date it is treated as ancient: it is a better
            // candidate for deletion than for taking up space forever.
            let when = meta.modified().unwrap_or(UNIX_EPOCH);
            Some((when, meta.len(), path))
        })
        .collect();

    // Newest first: the ones that survive are the latest. The cap is kept
    // per phone, so the count is one per serial.
    logs.sort_by(|a, b| b.0.cmp(&a.0));

    let mut running: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for (_, size, path) in logs {
        let total = running.entry(serial_of_log(&path)).or_insert(0);
        *total = total.saturating_add(size);
        if *total > keep_bytes {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Creates a new session's log, sharing read and write
/// but NOT delete.
///
/// Anyone must be able to read while the session runs (the app, an editor).
/// Deleting must not: that is
/// what keeps another engine's cap sweep from taking this session's log.
fn open_log(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    options.open(path)
}

impl SessionLog {
    pub fn create(serial: &str) -> Result<Self, String> {
        let directory = log_directory()?;
        // Before opening this session's log, bring the folder under the cap.
        // Here and not at close: close is not always reached (an engine that
        // dies does not clean up), and open always is.
        sweep_old_logs(&directory, MAX_LOG_BYTES_PER_PHONE);
        let unix_ms = unix_time_ms();
        let safe_serial = sanitize_filename(serial);
        let path = directory.join(format!(
            "mirryl-{unix_ms}-{}-{safe_serial}.jsonl",
            std::process::id()
        ));
        let file = open_log(&path).map_err(|error| {
            format!(
                "could not create the diagnostics log {}: {error}",
                path.display()
            )
        })?;
        let latest = directory.join("ULTIMO_LOG.txt");
        let _ = std::fs::write(&latest, path.to_string_lossy().as_bytes());
        Ok(Self {
            path,
            serial: serial.to_owned(),
            started: Instant::now(),
            file: Mutex::new(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn event(&self, component: &str, event: &str, fields: &str) {
        let mut line = format!(
            "{{\"unix_ms\":{},\"elapsed_ms\":{},\"pid\":{},\"serial\":\"{}\",\
\"component\":\"{}\",\"event\":\"{}\"",
            unix_time_ms(),
            self.started.elapsed().as_millis(),
            std::process::id(),
            json_escape(&self.serial),
            json_escape(component),
            json_escape(event)
        );
        if !fields.is_empty() {
            line.push(',');
            line.push_str(fields);
        }
        line.push_str("}\n");
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }

    pub fn message(&self, component: &str, event: &str, message: &str) {
        self.event(
            component,
            event,
            &format!("\"message\":\"{}\"", json_escape(message)),
        );
    }
}

pub fn json_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output
}

/// Where the session logs go.
///
/// First the folder the app names in `MIRRYL_LOG_DIR`: the app is the one
/// that decides where the engine's state lives, same as the transport file
/// or the window position: the engine's own folder can be read-only (a Store
/// or Linux package), and the `%TEMP%` fallback is cleaned by Windows and
/// looked at by nobody. Without the variable - the engine started by hand -
/// the folder beside the engine is used.
fn log_directory() -> Result<PathBuf, String> {
    if let Some(directory) = std::env::var_os("MIRRYL_LOG_DIR").map(PathBuf::from)
        && std::fs::create_dir_all(&directory).is_ok()
    {
        return Ok(directory);
    }
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    if let Some(directory) = executable_directory.map(|path| path.join("logs"))
        && std::fs::create_dir_all(&directory).is_ok()
    {
        return Ok(directory);
    }
    let fallback = std::env::temp_dir().join("Mirryl Connect").join("logs");
    std::fs::create_dir_all(&fallback)
        .map_err(|error| format!("could not create the diagnostics folder: {error}"))?;
    Ok(fallback)
}

fn sanitize_filename(value: &str) -> String {
    let output: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(72)
        .collect();
    if output.is_empty() {
        "android".to_owned()
    } else {
        output
    }
}

fn unix_time_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{json_escape, open_log, sanitize_filename, serial_of_log, sweep_old_logs};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    #[test]
    fn reads_the_serial_out_of_a_log_name() {
        assert_eq!(serial_of_log(Path::new("mirryl-1759000000000-4242-R5CX12ABCDE.jsonl")), "R5CX12ABCDE");
        // A serial with dashes (a sanitised mDNS name) is kept whole.
        assert_eq!(serial_of_log(Path::new("mirryl-1-2-adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp.jsonl")), "adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp");
        assert_eq!(serial_of_log(Path::new("s0.jsonl")), "");
    }

    /// The cap is per phone: with two serials and a cap of
    /// 25, each keeps its two newest logs, instead of the chattier one taking
    /// the other's room.
    #[test]
    fn sweep_caps_each_phone_on_its_own() {
        let dir = std::env::temp_dir().join(format!("mirryl-sweep-phone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let now = SystemTime::now();
        for (i, serial) in [(0, "AAA"), (1, "AAA"), (2, "AAA"), (3, "BBB"), (4, "AAA"), (5, "BBB"), (6, "BBB")] {
            let path = dir.join(format!("mirryl-{i}-1-{serial}.jsonl"));
            std::fs::write(&path, [b'x'; 10]).unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(now - Duration::from_secs(3600 * (10 - i))).unwrap();
        }

        sweep_old_logs(&dir, 25);

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(left, ["mirryl-2-1-AAA.jsonl", "mirryl-4-1-AAA.jsonl", "mirryl-5-1-BBB.jsonl", "mirryl-6-1-BBB.jsonl"]);
    }

    #[cfg(windows)]
    #[test]
    fn sweep_cannot_delete_a_log_that_is_still_open() {
        let dir = std::env::temp_dir().join(format!("mirryl-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let live = dir.join("open.jsonl");
        let closed = dir.join("closed.jsonl");
        use std::io::Write;
        let mut handle = open_log(&live).unwrap();
        handle.write_all(&[b'x'; 10]).unwrap();
        open_log(&closed).unwrap().write_all(&[b'x'; 10]).unwrap();

        // Cap 0 and both with content: the sweep tries to delete both.
        sweep_old_logs(&dir, 0);

        let live_survived = live.exists();
        let closed_survived = closed.exists();
        drop(handle);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(live_survived, "the open log must not be deletable");
        assert!(!closed_survived, "the closed log must be deleted");
    }

    #[test]
    fn sweep_keeps_newest_logs_under_the_cap() {
        let dir = std::env::temp_dir().join(format!("mirryl-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Five 10-byte logs, from the oldest (0) to the newest (4), and a file
        // that is not a log: with a cap of 25 the two newest stay.
        let now = SystemTime::now();
        for i in 0..5u64 {
            let path = dir.join(format!("s{i}.jsonl"));
            std::fs::write(&path, [b'x'; 10]).unwrap();
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(now - Duration::from_secs(3600 * (5 - i))).unwrap();
        }
        std::fs::write(dir.join("other.txt"), [b'x'; 100]).unwrap();

        sweep_old_logs(&dir, 25);

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(left, ["other.txt", "s3.jsonl", "s4.jsonl"]);
    }

    #[test]
    fn escapes_json_log_values() {
        assert_eq!(json_escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }

    #[test]
    fn creates_safe_short_log_names() {
        assert_eq!(sanitize_filename("adb:phone/one"), "adb_phone_one");
        assert!(sanitize_filename(&"x".repeat(100)).len() <= 72);
    }
}
