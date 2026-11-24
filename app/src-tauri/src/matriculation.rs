// ---------- Phone matriculation ----------
//
// A per-PC signing key and a PERMANENT, per-phone binding, beside the
// shared-constant seal (SEAL_PEPPER in main.rs). It closes two problems that a
// fixed pepper cannot:
//
//   1. The self-locking renewal. The seal expires after 30 days and the only
//      renewal channel is `adb push`, which itself needs Wireless debugging
//      already on - a phone that goes quiet for a month loses its stamp and
//      has no remote way to get a new one. Matriculation never expires, so
//      there is nothing to renew and nothing to lock.
//   2. The forgeable pepper. SEAL_PEPPER is inside the APK, so anyone who
//      decompiles it once can forge a valid stamp for any phone, forever,
//      without a PC ever being involved. Each PC signs with its own private
//      key, generated locally on first use and never shared - decompiling the
//      APK yields no secret at all.
//
// Everything here stays on the existing adb channel: no server, no local
// network listener, nothing leaves the LAN.
//
// THIS FILE IS THE PC HALF. The Android half is WifiAdbHelper.java in the
// phone app. `crate::push_seal` (called from both sites this module is called
// from) keeps phones whose APK does not write `.devid` working; this module
// ADDS a second, independent stamp for phones running an APK that does.
//
// ---- Wire format, for the Android side to match exactly ----
//
// The phone is expected to:
//   - Generate a random device id (e.g. UUID) the first time it ever runs,
//     store it in its OWN PRIVATE storage (SharedPreferences or a private
//     file - NOT external storage), and treat it as immutable forever after.
//   - Also write a plain copy of that same id to
//     `/sdcard/Android/data/com.mirryl.app/files/.devid` - this is a
//     write-only EXPORT for the PC to read during pairing. The app must
//     never read this external copy back as its own identity; the private
//     copy is the only one it trusts about itself.
//   - Read `/sdcard/Android/data/com.mirryl.app/files/.mcert` (written by
//     `matriculate` below) as `{"v":2,"dev":"<device id>","pc":"<PC public
//     key, base64, SEC1 compressed point>","sig":"<ECDSA-P256-SHA256
//     signature over "dev:pc" as UTF-8 bytes, base64, DER-encoded>"}`.
//   - On the FIRST valid file it ever sees (dev matches its own private id,
//     AND sig verifies under the embedded pc key), PIN that pc key
//     permanently into its OWN PRIVATE storage and accept it.
//   - On every check after that, only accept a `.mcert` whose `pc` field
//     matches the PINNED key - a `.mcert` signed by a DIFFERENT key (e.g.
//     pushed by a different PC) must be ignored, not adopted, even though
//     external storage is trivially writable by anyone with adb access.
//
// This last point is the one subtlety that is easy to get wrong: pinning
// MUST live in private storage and MUST only ever be written once. If the
// "who am I pinned to" value itself lived in external storage (or got
// overwritten on every check from whatever `.mcert` happens to be sitting
// there), a second PC could simply push its own self-consistent `pc`+`sig`
// pair and re-pin the phone at will - the whole point would be lost. The
// external file is an untrusted inbox; the phone's own private state is the
// only thing doing the actual locking.

use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::Manager;

const DEVID_REMOTE: &str = "/sdcard/Android/data/com.mirryl.app/files/.devid";
const MCERT_REMOTE: &str = "/sdcard/Android/data/com.mirryl.app/files/.mcert";

/// On-disk shape of pc_identity.json - the only copy of this PC's private
/// signing key. Never regenerated once it exists: doing so would silently
/// orphan every phone already pinned to the old one (see the wire-format note
/// above - a phone only ever trusts the FIRST key it pins to).
///
/// Two generations of the file coexist, which is why both key fields are
/// optional: a reader that only understood the new one would have to treat
/// every 1.0.0 install's file as unreadable, and "unreadable" here must never
/// mean "regenerate" (see `signing_key`). The old file is read once, its key
/// kept, and the file rewritten in the new form - see `load_or_create_identity`.
#[derive(Serialize, Deserialize, Default)]
struct PcIdentity {
    /// Format version. Absent in 1.0.0 files, `2` from 1.0.1 on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    v: Option<u32>,
    /// v1 (1.0.0): PKCS#8 DER, base64, IN THE CLEAR. With the ACL the file
    /// inherits from %APPDATA% another local account could simply read it,
    /// and with it forge this PC's matriculation stamp for any phone. Read for
    /// migration only; never written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    private_key_b64: Option<String>,
    /// v2: the same PKCS#8 DER wrapped by DPAPI (CryptProtectData, current
    /// user's scope, no extra entropy), base64. Only this Windows account can
    /// unwrap it - a copy of the file, or a read from another account, yields
    /// nothing usable. The file's own DACL (`restrict_to_owner`) is the second,
    /// independent layer: nobody but this account, SYSTEM and Administrators
    /// can open it in the first place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    private_key_dpapi_b64: Option<String>,
}

const PC_IDENTITY_FORMAT_VERSION: u32 = 2;

/// A key read back from disk, plus whether the file it came from still needs
/// rewriting in the current format.
struct LoadedIdentity {
    key: SigningKey,
    /// True when the file was the 1.0.0 clear-text form.
    legacy: bool,
}

fn identity_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("pc_identity.json"))
}

/// Loads this PC's signing key, generating and persisting one the first
/// time it is ever needed. One key for the lifetime of this install.
///
/// A file that EXISTS but cannot be read back is an error, never a reason to
/// mint a new key: a failure anywhere in the read/decode/parse chain (a file
/// truncated by a power cut, broken base64, invalid DER) that fell through to
/// `SigningKey::random` would OVERWRITE the file, permanently breaking the
/// link to every phone already matriculated, with no message and no way back.
/// Only a genuinely missing file means "first run".
///
/// The single caller logs the error and skips matriculation for this
/// connection, which is the right outcome: nothing is destroyed, and the next
/// run tries again against the file that is still there to be recovered.
fn signing_key(app: &tauri::AppHandle) -> Result<SigningKey, String> {
    let path = identity_path(app)?;
    let (key, migration_warning) = load_or_create_identity(&path)?;
    if let Some(warning) = migration_warning {
        crate::log_activity(&format!("matriculation: {warning}"));
    }
    Ok(key)
}

/// Loads - and therefore migrates - this PC's identity once at startup,
/// logging any failure and ignoring the key itself.
///
/// The DPAPI + owner-only-DACL rewrite of a 1.0.0 clear-text pc_identity.json
/// lives inside `signing_key`, which `matriculate` only reaches when a phone
/// actually connects. Hardening a file at rest must not depend on someone
/// plugging in a phone, so `.setup()` calls this once per launch.
///
/// Idempotent and cheap: a v2 file is read, reported as non-legacy and left
/// exactly as it was; only a 1.0.0 file is rewritten, and only once. On a PC
/// that never had an identity this DOES create one, because
/// `load_or_create_identity` treats a missing file as first run. That is
/// deliberate and harmless: the key is generated locally, is written
/// encrypted under an owner-only DACL, never leaves the machine, and is the
/// very same key the first matriculation would have minted anyway.
pub fn migrate_identity_if_needed(app: &tauri::AppHandle) {
    if let Err(e) = signing_key(app) {
        // Same wording as the `matriculate` call site: signing_key refuses to
        // overwrite a pc_identity.json it cannot read, so the log is the only
        // trace that a recoverable identity file went bad.
        crate::log_activity(&format!("matriculation: {e}"));
    }
}

/// The whole read / migrate / create decision, on a plain path so the tests can
/// drive it against a file in %TEMP% instead of needing a tauri::AppHandle.
///
/// Returns the key and, when a 1.0.0 clear-text file could not be rewritten in
/// the new format, a warning for the caller to log. The warning is NOT an
/// error on purpose: the key was read fine and is the one every phone is
/// pinned to, so it gets used regardless. The old file stays as it was and the
/// next run tries the migration again.
fn load_or_create_identity(path: &Path) -> Result<(SigningKey, Option<String>), String> {
    match read_identity(path)? {
        Some(LoadedIdentity { key, legacy: false }) => Ok((key, None)),
        Some(LoadedIdentity { key, legacy: true }) => {
            // 1.0.0 file: same key, rewritten encrypted and with its own DACL.
            let der = key
                .to_pkcs8_der()
                .map_err(|e| format!("could not re-encode existing signing key: {e}"))?;
            let warning = write_identity(path, der.as_bytes()).err().map(|e| {
                format!(
                    "pc_identity.json is still in the 1.0.0 clear-text format and could not be \
                     rewritten encrypted ({e}); the existing key is still in use and will NOT be \
                     replaced - the migration is retried on the next connection"
                )
            });
            Ok((key, warning))
        }
        // The one case that really is "first run on this install".
        None => {
            let key = SigningKey::random(&mut OsRng);
            let der = key
                .to_pkcs8_der()
                .map_err(|e| format!("could not encode new signing key: {e}"))?;
            write_identity(path, der.as_bytes())?;
            Ok((key, None))
        }
    }
}

/// Reads and decodes pc_identity.json. `Ok(None)` only for a file that does
/// not exist; every other failure is an error, because (see `signing_key`) a
/// file that EXISTS but cannot be read back must never be replaced. That
/// includes a v2 blob DPAPI refuses to unwrap - typically the file was copied
/// from, or is being read by, a different Windows account - and a file that
/// somehow carries neither key field.
fn read_identity(path: &Path) -> Result<Option<LoadedIdentity>, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "pc_identity.json could not be read ({e}); refusing to replace it"
            ))
        }
    };
    let identity: PcIdentity = serde_json::from_str(&contents).map_err(|e| {
        format!("pc_identity.json exists but is not valid JSON ({e}); refusing to replace it - a new key would orphan every phone already matriculated to this PC")
    })?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let (der, legacy) = if let Some(blob_b64) = &identity.private_key_dpapi_b64 {
        let blob = b64.decode(blob_b64).map_err(|e| {
            format!("pc_identity.json exists but its encrypted key is not valid base64 ({e}); refusing to replace it")
        })?;
        let der = unprotect_der(&blob).map_err(|e| {
            format!("pc_identity.json exists but its key could not be decrypted ({e}) - it belongs to this Windows account only, a copy from another account or PC cannot be used; refusing to replace it")
        })?;
        (der, false)
    } else if let Some(der_b64) = &identity.private_key_b64 {
        let der = b64.decode(der_b64).map_err(|e| {
            format!("pc_identity.json exists but its key is not valid base64 ({e}); refusing to replace it")
        })?;
        (der, true)
    } else {
        return Err(
            "pc_identity.json exists but holds no key at all; refusing to replace it".to_string(),
        );
    };
    let key = SigningKey::from_pkcs8_der(&der).map_err(|e| {
        format!("pc_identity.json exists but its key is not valid PKCS#8 ({e}); refusing to replace it")
    })?;
    Ok(Some(LoadedIdentity { key, legacy }))
}

/// Persists a PKCS#8 DER as a v2 pc_identity.json: DPAPI-wrapped content,
/// owner-only DACL, atomic replace.
///
/// Order matters. The content is encrypted BEFORE it touches the disk, so even
/// the temp file never holds the key in the clear. The DACL is then applied to
/// the temp file BEFORE the rename: a rename within the same directory keeps
/// the file's own security descriptor, so the final pc_identity.json is never
/// observable, for any window of time, with the inherited %APPDATA% ACL that
/// let another account read the 1.0.0 file. Doing it the other way round
/// (rename, then fix the ACL) would leave exactly that gap, and a crash in
/// between would leave it permanently.
///
/// Temp file + rename rather than a plain fs::write, the way write_devices()
/// does: fs::write truncates first, so a power cut mid-write would leave a
/// zero-length pc_identity.json.
fn write_identity(path: &Path, der: &[u8]) -> Result<(), String> {
    let blob = protect_der(der)?;
    let identity = PcIdentity {
        v: Some(PC_IDENTITY_FORMAT_VERSION),
        private_key_b64: None,
        private_key_dpapi_b64: Some(base64::engine::general_purpose::STANDARD.encode(blob)),
    };
    let json = serde_json::to_string(&identity).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
    if let Err(e) = restrict_to_owner(&tmp) {
        // Never leave a half-done temp file behind to be confused with a real
        // one; the existing pc_identity.json (if any) is untouched.
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(windows)]
fn utf16z(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn last_error() -> u32 {
    // SAFETY: plain thread-local read, no arguments.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

/// DPAPI-wraps `der` for the current Windows user. No description string, no
/// extra entropy: the file's DACL is the second layer, and entropy baked into
/// the binary would be as public as SEAL_PEPPER is. CRYPTPROTECT_UI_FORBIDDEN
/// because this runs on a background
/// connection path with no one to click a prompt. Not CRYPTPROTECT_LOCAL_MACHINE:
/// machine scope would let any account on this PC unwrap it, which is exactly
/// the exposure being closed.
#[cfg(windows)]
fn protect_der(der: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: der.len() as u32,
        pbData: der.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: `input` points at `der`, alive for the whole call; every optional
    // pointer is null as the API allows; `output` is filled by the call and
    // its buffer released with LocalFree below, as documented.
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("CryptProtectData failed (error {})", last_error()));
    }
    // SAFETY: on success pbData/cbData describe a LocalAlloc'd buffer we own.
    let blob = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        bytes
    };
    Ok(blob)
}

#[cfg(windows)]
fn unprotect_der(blob: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: blob.len() as u32,
        pbData: blob.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: as in protect_der. The description out-param is null: we never
    // stored one and do not want to own a second LocalAlloc'd buffer.
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!("CryptUnprotectData failed (error {})", last_error()));
    }
    // SAFETY: on success pbData/cbData describe a LocalAlloc'd buffer we own.
    let der = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        LocalFree(output.pbData as _);
        bytes
    };
    Ok(der)
}

/// Replaces the file's DACL with a protected (non-inheriting) one holding
/// exactly three full-control entries: the account running this process,
/// SYSTEM and BUILTIN\Administrators. Nothing inherited from %APPDATA%, whose
/// ACL can hand read access to other accounts (a sandbox group, for one).
/// SYSTEM and Administrators are kept because an administrator can take
/// ownership anyway; excluding them buys nothing and breaks backup tools.
#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> Result<(), String> {
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, GetTokenInformation, TokenUser, WinBuiltinAdministratorsSid,
        WinLocalSystemSid, ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE,
        PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // 1. The SID of the account this process runs as, straight from its token
    //    (not from a user name: no lookup, no localisation, no ambiguity).
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing;
    // the token handle is closed below on every path.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(format!("OpenProcessToken failed (error {})", last_error()));
    }
    let mut needed: u32 = 0;
    // SAFETY: first call with a zero-length buffer only reports the size.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
    let mut user_buf = vec![0u8; needed as usize];
    // SAFETY: `user_buf` is exactly the size the previous call asked for.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            user_buf.as_mut_ptr() as *mut _,
            needed,
            &mut needed,
        )
    };
    // SAFETY: `token` came from a successful OpenProcessToken above.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(format!("GetTokenInformation failed (error {})", last_error()));
    }
    // SAFETY: a successful TokenUser query fills the buffer with a TOKEN_USER
    // whose Sid points inside that same buffer, which outlives every use below.
    let user_sid = unsafe { (*(user_buf.as_ptr() as *const TOKEN_USER)).User.Sid };

    // 2. The two well-known SIDs, by type rather than by name (S-1-5-18 and
    //    S-1-5-32-544 on every Windows, whatever the display language).
    let mut system_sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    let mut admins_sid = [0u8; SECURITY_MAX_SID_SIZE as usize];
    for (kind, buf, name) in [
        (WinLocalSystemSid, &mut system_sid, "SYSTEM"),
        (WinBuiltinAdministratorsSid, &mut admins_sid, "Administrators"),
    ] {
        let mut len = SECURITY_MAX_SID_SIZE;
        // SAFETY: `buf` holds SECURITY_MAX_SID_SIZE bytes, the documented
        // upper bound for any SID; `len` is passed in as that size.
        if unsafe { CreateWellKnownSid(kind, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _, &mut len) }
            == 0
        {
            return Err(format!("CreateWellKnownSid({name}) failed (error {})", last_error()));
        }
    }

    // 3. Three explicit, non-inheritable, full-control entries and nothing else:
    //    `oldacl` is null, so the new DACL is built from this list alone.
    let entry = |sid: *mut std::ffi::c_void, kind| EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: kind,
            ptstrName: sid as *mut u16,
        },
    };
    let entries = [
        entry(user_sid, TRUSTEE_IS_USER),
        entry(system_sid.as_mut_ptr() as *mut _, TRUSTEE_IS_WELL_KNOWN_GROUP),
        entry(admins_sid.as_mut_ptr() as *mut _, TRUSTEE_IS_WELL_KNOWN_GROUP),
    ];
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `entries` and every SID it points at are alive for the call; the
    // resulting ACL is LocalAlloc'd and freed below.
    let err = unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), std::ptr::null(), &mut dacl) };
    if err != ERROR_SUCCESS {
        return Err(format!("SetEntriesInAclW failed (error {err})"));
    }

    // 4. PROTECTED_DACL_SECURITY_INFORMATION is the "no inheritance" bit: it
    //    sets SE_DACL_PROTECTED so nothing from the parent directory is merged
    //    in, now or on a later inheritance propagation.
    let wide = utf16z(path);
    // SAFETY: `wide` is NUL-terminated and alive for the call; `dacl` is valid.
    let err = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    // SAFETY: `dacl` was allocated by SetEntriesInAclW with LocalAlloc.
    unsafe { LocalFree(dacl as _) };
    if err != ERROR_SUCCESS {
        return Err(format!("SetNamedSecurityInfoW failed (error {err})"));
    }
    Ok(())
}

// No DPAPI and no NTFS DACLs off Windows, so the Linux build never persists a
// key: it hits the error and skips matriculation, and its phones stay on the
// seal - never regenerate, never write a clear-text file.
#[cfg(not(windows))]
fn protect_der(_der: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI is only available on Windows".to_string())
}

#[cfg(not(windows))]
fn unprotect_der(_blob: &[u8]) -> Result<Vec<u8>, String> {
    Err("DPAPI is only available on Windows".to_string())
}

#[cfg(not(windows))]
fn restrict_to_owner(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// True for the one shape a phone's `.devid` ever has: the `UUID.randomUUID()`
/// the APK writes, 36 chars, hex and four dashes. Anything else that comes
/// back from `adb shell cat` is adb talking, not the phone.
///
/// A blacklist ("No such file", "error") would let through a line such as
/// `adb.exe: device '192.168.0.230:41161' not found` - a phone answering the
/// app's connect and dropping off before the cat - and "matriculate" it.
fn is_device_id(candidate: &str) -> bool {
    candidate.len() == 36
        && candidate.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// What `matriculate` did, for the callers that can tell the user
/// (connect_and_grant and start_mirror). Every variant is still "never fails
/// the caller".
#[derive(Debug, PartialEq)]
pub enum MatriculationOutcome {
    /// The phone has no `.devid` to sign against (an APK that does not write
    /// one, or one that has not run yet): only the seal went out.
    SealOnly,
    /// Certificate pushed (or re-pushed).
    Matriculated,
    /// The identity file could not be used; logged, seal pushed.
    IdentityUnavailable,
}

/// Best-effort, exactly like the `push_seal` this supplements: never blocks
/// or fails the caller. A phone that ends up unmatriculated on a given attempt
/// just needs another connection later - once it does succeed, unlike the
/// seal, it never needs another.
pub fn matriculate(app: &tauri::AppHandle, adb: &str, target: &str) -> MatriculationOutcome {
    // The seal first, so every phone keeps working regardless of anything
    // below.
    crate::push_seal(adb, target);

    let device_id = crate::run_process(adb, &["-s", target, "shell", "cat", DEVID_REMOTE]);
    let device_id = device_id.trim();
    // An APK that does not write .devid, or one that has not run once yet to
    // write it - nothing to sign against. The seal above already
    // covers this phone; the next connection after the phone writes its id
    // picks it up automatically, no separate migration step needed. Also
    // every message adb itself prints instead of the file (see is_device_id).
    if !is_device_id(device_id) {
        return MatriculationOutcome::SealOnly;
    }

    // Logged, not swallowed: signing_key refuses to overwrite a
    // pc_identity.json it cannot read, so this branch is the only trace that a
    // recoverable identity file went bad. Silence here would look exactly like
    // "matriculation just stopped working".
    let key = match signing_key(app) {
        Ok(key) => key,
        Err(e) => {
            crate::log_activity(&format!("matriculation: {e}"));
            return MatriculationOutcome::IdentityUnavailable;
        }
    };
    let verifying = VerifyingKey::from(&key);
    let pc_pub_b64 = base64::engine::general_purpose::STANDARD
        .encode(verifying.to_encoded_point(true).as_bytes());
    let payload = format!("{device_id}:{pc_pub_b64}");
    let signature: Signature = key.sign(payload.as_bytes());
    let sig_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_der().as_bytes());

    let content = format!("{{\"v\":2,\"dev\":\"{device_id}\",\"pc\":\"{pc_pub_b64}\",\"sig\":\"{sig_b64}\"}}");
    // Same private staging as the seal (see stage_private_file in main.rs).
    let Some(path) = crate::stage_private_file("mcert", &content) else {
        return MatriculationOutcome::IdentityUnavailable;
    };
    let _ = crate::run_process(adb, &["-s", target, "push", &path.to_string_lossy(), MCERT_REMOTE]);
    let _ = std::fs::remove_file(&path);

    MatriculationOutcome::Matriculated
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;

    /// adb's own error lines, and near misses of the UUID shape.
    #[test]
    fn device_id_accepts_only_the_uuid_the_apk_writes() {
        assert!(is_device_id("38b4a0db-66bf-49c8-aae5-814123aed9f8"));
        assert!(is_device_id("8273D1C4-18C1-4CAC-8C46-F9D35C0C5EC7"));
        assert!(!is_device_id("adb.exe: device '192.168.0.230:41161' not found"));
        assert!(!is_device_id("cat: /sdcard/Android/data/com.mirryl.app/files/.devid: No such file or directory"));
        assert!(!is_device_id("error: device offline"));
        assert!(!is_device_id(""));
        assert!(!is_device_id("38b4a0db-66bf-49c8-aae5-814123aed9f"));
        assert!(!is_device_id("38b4a0db-66bf-49c8-aae5-814123aed9f8\n"));
        assert!(!is_device_id("38b4a0db_66bf_49c8_aae5_814123aed9f8"));
    }

    /// Confirms the exact bytes an Android implementation needs to
    /// reproduce: sign "dev:pc_pub_b64" and verify it comes back clean with
    /// a fresh key, independent of any file I/O or app state.
    #[test]
    fn signs_and_verifies_a_payload_round_trip() {
        let key = SigningKey::random(&mut OsRng);
        let verifying = VerifyingKey::from(&key);
        let pc_pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(verifying.to_encoded_point(true).as_bytes());
        let payload = format!("{}:{}", "test-device-id", pc_pub_b64);
        let signature: Signature = key.sign(payload.as_bytes());
        assert!(verifying.verify(payload.as_bytes(), &signature).is_ok());
    }

    /// A signature made for one device id must not verify against a
    /// different one - this is the whole binding the design relies on.
    #[test]
    fn signature_does_not_verify_for_a_different_device_id() {
        let key = SigningKey::random(&mut OsRng);
        let verifying = VerifyingKey::from(&key);
        let pc_pub_b64 =
            base64::engine::general_purpose::STANDARD.encode(verifying.to_encoded_point(true).as_bytes());
        let signature: Signature = key.sign(format!("device-a:{pc_pub_b64}").as_bytes());
        assert!(verifying
            .verify(format!("device-b:{pc_pub_b64}").as_bytes(), &signature)
            .is_err());
    }

    /// A signature from one PC's key must not verify under a different PC's
    /// key - this is what stops a second PC from forging a certificate that
    /// looks like it came from the phone's already-pinned PC.
    #[test]
    fn signature_does_not_verify_under_a_different_pc_key() {
        let key_a = SigningKey::random(&mut OsRng);
        let key_b = SigningKey::random(&mut OsRng);
        let verifying_b = VerifyingKey::from(&key_b);
        let payload = "some-device-id:whatever";
        let signature: Signature = key_a.sign(payload.as_bytes());
        assert!(verifying_b.verify(payload.as_bytes(), &signature).is_err());
    }

    /// A scratch pc_identity.json under %TEMP%, removed on drop. Each test
    /// names its own so they can run in parallel.
    #[cfg(windows)]
    struct ScratchIdentity(PathBuf);

    #[cfg(windows)]
    impl ScratchIdentity {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("mirryl-test-pc_identity-{}-{name}.json", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }
    }

    #[cfg(windows)]
    impl Drop for ScratchIdentity {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(self.0.with_extension("json.tmp"));
        }
    }

    #[cfg(windows)]
    fn fresh_der() -> Vec<u8> {
        SigningKey::random(&mut OsRng)
            .to_pkcs8_der()
            .expect("encode")
            .as_bytes()
            .to_vec()
    }

    /// The write path (DPAPI + DACL + rename) and the read path agree: the
    /// exact DER that went in comes back out, and the file on disk is the v2
    /// shape with no clear-text key field at all.
    #[cfg(windows)]
    #[test]
    fn identity_dpapi_round_trip_and_v2_shape_on_disk() {
        let scratch = ScratchIdentity::new("roundtrip");
        let der = fresh_der();
        write_identity(&scratch.0, &der).expect("write");

        let loaded = read_identity(&scratch.0).expect("read").expect("present");
        assert!(!loaded.legacy);
        assert_eq!(loaded.key.to_pkcs8_der().unwrap().as_bytes(), der.as_slice());

        let raw = std::fs::read_to_string(&scratch.0).unwrap();
        let on_disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(on_disk["v"], PC_IDENTITY_FORMAT_VERSION);
        assert!(on_disk.get("private_key_b64").is_none(), "clear-text field must be gone: {raw}");
        let blob_b64 = on_disk["private_key_dpapi_b64"].as_str().expect("dpapi field");
        let blob = base64::engine::general_purpose::STANDARD.decode(blob_b64).unwrap();
        // The DER must not appear verbatim inside the stored blob either.
        assert!(!blob.windows(der.len()).any(|w| w == der.as_slice()));
        assert!(!scratch.0.with_extension("json.tmp").exists(), "temp file left behind");
    }

    /// The file's DACL is its own (no "(I)" inherited entries) and names
    /// exactly three principals: this account, SYSTEM and Administrators.
    /// Checked through icacls.
    #[cfg(windows)]
    #[test]
    fn identity_file_dacl_is_owner_system_admins_only() {
        let scratch = ScratchIdentity::new("dacl");
        write_identity(&scratch.0, &fresh_der()).expect("write");

        let out = std::process::Command::new("icacls")
            .arg(&scratch.0)
            .output()
            .expect("icacls");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let aces: Vec<String> = text
            .lines()
            .filter(|l| l.contains(":("))
            .map(|l| {
                // First line is "<path> <principal>:(perms)", the rest are
                // indented "<principal>:(perms)". Keep only the ACE part.
                let l = l.trim();
                let l = l.strip_prefix(&*scratch.0.to_string_lossy()).unwrap_or(l);
                l.trim().to_string()
            })
            .collect();
        assert_eq!(aces.len(), 3, "expected exactly three ACEs, icacls said:\n{text}");
        assert!(!text.contains("(I)"), "inherited ACE present:\n{text}");
        let has = |needle: &str| aces.iter().any(|a| a.eq_ignore_ascii_case(needle));
        assert!(has("NT AUTHORITY\\SYSTEM:(F)"), "SYSTEM missing:\n{text}");
        assert!(has("BUILTIN\\Administrators:(F)"), "Administrators missing:\n{text}");
        let me = format!(
            "{}\\{}:(F)",
            std::env::var("USERDOMAIN").unwrap_or_default(),
            std::env::var("USERNAME").unwrap_or_default()
        );
        assert!(has(&me), "current account {me} missing:\n{text}");
    }

    /// A 1.0.0 clear-text file: the SAME key is returned (never a new one),
    /// the file is rewritten in the v2 shape, and reading it again still
    /// yields that key with no further migration.
    #[cfg(windows)]
    #[test]
    fn legacy_clear_text_identity_is_migrated_keeping_the_same_key() {
        let scratch = ScratchIdentity::new("migration");
        let der = fresh_der();
        let legacy = format!(
            "{{\"private_key_b64\":\"{}\"}}",
            base64::engine::general_purpose::STANDARD.encode(&der)
        );
        std::fs::write(&scratch.0, legacy).unwrap();

        let (key, warning) = load_or_create_identity(&scratch.0).expect("load");
        assert!(warning.is_none(), "migration reported a problem: {warning:?}");
        assert_eq!(key.to_pkcs8_der().unwrap().as_bytes(), der.as_slice());

        let raw = std::fs::read_to_string(&scratch.0).unwrap();
        let on_disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(on_disk["v"], PC_IDENTITY_FORMAT_VERSION);
        assert!(on_disk.get("private_key_b64").is_none(), "still clear-text: {raw}");
        assert!(on_disk.get("private_key_dpapi_b64").is_some());

        let loaded = read_identity(&scratch.0).expect("read").expect("present");
        assert!(!loaded.legacy);
        let (again, warning) = load_or_create_identity(&scratch.0).expect("load again");
        assert!(warning.is_none());
        assert_eq!(again.to_pkcs8_der().unwrap().as_bytes(), der.as_slice());
    }

    /// A v2 file whose blob DPAPI cannot unwrap (here: not a DPAPI blob at
    /// all, standing in for "another account's file") is an error, and the
    /// file is left exactly as it was - never overwritten with a new key.
    #[cfg(windows)]
    #[test]
    fn undecryptable_v2_identity_is_an_error_and_is_never_replaced() {
        let scratch = ScratchIdentity::new("undecryptable");
        let bogus = format!(
            "{{\"v\":2,\"private_key_dpapi_b64\":\"{}\"}}",
            base64::engine::general_purpose::STANDARD.encode(b"definitely not a DPAPI blob")
        );
        std::fs::write(&scratch.0, &bogus).unwrap();

        let err = load_or_create_identity(&scratch.0).expect_err("must refuse");
        assert!(err.contains("could not be decrypted"), "{err}");
        assert_eq!(std::fs::read_to_string(&scratch.0).unwrap(), bogus);
    }

    /// Only a genuinely absent file is "first run": a key gets created and
    /// persisted, and the next load returns that same key.
    #[cfg(windows)]
    #[test]
    fn absent_identity_is_created_once_and_then_reused() {
        let scratch = ScratchIdentity::new("firstrun");
        let (first, warning) = load_or_create_identity(&scratch.0).expect("create");
        assert!(warning.is_none());
        assert!(scratch.0.exists());
        let (second, _) = load_or_create_identity(&scratch.0).expect("reload");
        assert_eq!(first.to_bytes(), second.to_bytes());
    }
}
