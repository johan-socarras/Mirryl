//! Own implementation of Android's "Pair device with QR code" flow, which the
//! standard `adb` CLI does not offer - it only pairs with the six-digit code,
//! already used elsewhere in this app via `adb pair`. In both flows the phone
//! runs the pairing server and the PC connects to it as the client.
//!
//! Ported from the public, Apache-2.0 AOSP `adb` source (not copied
//! verbatim - reimplemented from reading pairing_connection.cpp,
//! pairing_auth.cpp, aes_128_gcm.cpp, spake25519.cc, tls_connection.cpp,
//! client/adb_wifi.cpp, client/auth.cpp and adb_mdns.cpp) plus only
//! permissively-licensed crates (curve25519-dalek, rustls, mdns-sd, qrcode,
//! ...). No GPL code or dependency anywhere in this module.
//!
//! The SPAKE2 core (exact M/N curve constants, BoringSSL's cofactor-clearing
//! "hack", the transcript hash, and the AES-128-GCM key derivation) is pinned
//! by a known-answer test (`spake2_matches_known_answer_vector` below) whose
//! values come from a pairing that worked against a real phone.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes128Gcm, KeyInit, Nonce};
use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use hkdf::Hkdf;
use num_bigint::BigUint;
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256, Sha512};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ---------- Windows Firewall ----------
// mDNS discovery (inbound multicast UDP 5353) needs Windows Firewall to allow
// inbound traffic to *this* executable. The rule is created by
// `firewall_repair_script` in main.rs, which asks for administrator once, when
// the user presses Repair.

// ---------- Diagnostics ----------
// A line per pairing step in qr_pairing.log, in the Mirryl data folder, so a
// failed attempt on a real phone can be diagnosed after the fact.
// Best-effort: never let logging break pairing.
fn log_line(msg: &str) {
    // The writable data folder, like every other log (`local_data_dir`), not
    // the folder of the exe, which is read-only in the Linux package.
    if let Some(dir) = crate::local_data_dir() {
        let path = dir.join("qr_pairing.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(f, "[{}] {}", ts, msg);
        }
        // Same cap as watcher.log and activity.log: past 2 MB only the last
        // 500 lines stay.
        crate::trim_log_if_large(&path);
    }
}

// ---------- SPAKE2 core (see spake25519.cc for the exact algorithm this mirrors) ----------

// M and N generator points, exactly as published in BoringSSL's
// crypto/curve25519/spake25519.cc (compressed Edwards25519 encoding).
const N_POINT_HEX: &str = "10e3df0ae37d8e7a99b5fe74b44672103dbddcbd06af680d71329a11693bc778";
const M_POINT_HEX: &str = "5ada7e4bf6ddd9adb6626d32131c6b5c51a1e347a3478f53cfcf441b88eed12e";

fn decompress_point(hexstr: &str) -> EdwardsPoint {
    let bytes = hex_decode(hexstr);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    CompressedEdwardsY(arr)
        .decompress()
        .expect("hardcoded M/N point constants must be valid curve points")
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn curve_order() -> BigUint {
    // l = 2^252 + 27742317777372353535851937790883648493
    BigUint::parse_bytes(
        b"1000000000000000000000000000000014def9dea2f79cd65812631a5cf5d3ed",
        16,
    )
    .unwrap()
}

fn sc_reduce_64(data64: &[u8; 64]) -> BigUint {
    BigUint::from_bytes_le(data64) % curve_order()
}

/// Replicates BoringSSL's historical cofactor-clearing fix: rather than
/// wrapping back mod L, multiples of L are added (based on the bottom 3
/// bits) so the *unreduced* integer is a multiple of 8. This matters
/// because M/N may carry small-order torsion that only cancels out when
/// multiplied by a true multiple of 8, not merely `x mod 8 == 0` after a
/// fresh mod-L reduction (which would undo the whole point of this).
fn password_scalar_from_hash(password_hash_64: &[u8; 64]) -> BigUint {
    let l = curve_order();
    let mut v = sc_reduce_64(password_hash_64);
    if (&v & BigUint::from(1u8)) == BigUint::from(1u8) {
        v += &l;
    }
    if (&v >> 1u32) & BigUint::from(1u8) == BigUint::from(1u8) {
        v += &l * 2u8;
    }
    if (&v >> 2u32) & BigUint::from(1u8) == BigUint::from(1u8) {
        v += &l * 4u8;
    }
    debug_assert_eq!(&v & BigUint::from(7u8), BigUint::from(0u8));
    v
}

/// Scalar multiplication using the raw (possibly-unreduced-mod-L) integer,
/// needed specifically for the M/N mask points - see password_scalar_from_hash.
/// Not constant-time; acceptable here since this runs at most a handful of
/// times per pairing attempt, not as a repeated oracle.
fn scalarmult_raw(point: EdwardsPoint, scalar: &BigUint) -> EdwardsPoint {
    let bits = scalar.bits();
    let mut result = ED25519_BASEPOINT_POINT * Scalar::ZERO; // identity
    for i in (0..bits).rev() {
        result += result;
        if scalar.bit(i) {
            result += point;
        }
    }
    result
}

fn biguint_to_scalar_mod_l(v: &BigUint) -> Scalar {
    let r = v % curve_order();
    let mut bytes = r.to_bytes_le();
    bytes.resize(32, 0);
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Scalar::from_bytes_mod_order(arr)
}

struct Spake2Msg {
    encoded: [u8; 32],
    private_key: Scalar,
    password_scalar: BigUint,
    password_hash_64: [u8; 64],
}

/// role: true = "alice" (client), false = "bob" (server). This app only
/// ever plays the client role (the phone runs the pairing server after
/// scanning the QR, and the PC connects to it), but both branches are kept:
/// the known-answer test plays both sides, and the transcript hash direction
/// depends on the role.
fn spake2_generate_msg(is_alice: bool, password: &[u8], m: EdwardsPoint, n: EdwardsPoint) -> Spake2Msg {
    let mut ephemeral = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut ephemeral);
    spake2_generate_msg_with_random(is_alice, password, &ephemeral, m, n)
}

/// Same as spake2_generate_msg but with the ephemeral randomness supplied by
/// the caller, so the known-answer test below can drive it deterministically
/// while still exercising the exact same production code path.
fn spake2_generate_msg_with_random(
    is_alice: bool,
    password: &[u8],
    ephemeral: &[u8; 64],
    m: EdwardsPoint,
    n: EdwardsPoint,
) -> Spake2Msg {
    let x = sc_reduce_64(ephemeral);
    let private_key_big = &x * 8u8;
    let private_key = biguint_to_scalar_mod_l(&private_key_big);
    let p_point = ED25519_BASEPOINT_POINT * private_key;

    let mut hasher = Sha512::new();
    hasher.update(password);
    let hash = hasher.finalize();
    let mut password_hash_64 = [0u8; 64];
    password_hash_64.copy_from_slice(&hash);

    let password_scalar = password_scalar_from_hash(&password_hash_64);
    let mask_point = if is_alice { m } else { n };
    let mask = scalarmult_raw(mask_point, &password_scalar);
    let pstar = p_point + mask;

    Spake2Msg {
        encoded: pstar.compress().to_bytes(),
        private_key,
        password_scalar,
        password_hash_64,
    }
}

fn spake2_process_msg(
    is_alice: bool,
    their_msg: &[u8; 32],
    my: &Spake2Msg,
    my_name: &[u8],
    their_name: &[u8],
    m: EdwardsPoint,
    n: EdwardsPoint,
) -> Result<[u8; 64], String> {
    let qstar = CompressedEdwardsY(*their_msg)
        .decompress()
        .ok_or("peer's SPAKE2 point is not on the curve")?;
    let peers_mask_point = if is_alice { n } else { m };
    let peers_mask = scalarmult_raw(peers_mask_point, &my.password_scalar);
    let q = qstar + (-peers_mask);
    let dh_shared = q * my.private_key;
    let dh_shared_encoded = dh_shared.compress().to_bytes();

    let mut sha = Sha512::new();
    let lp = |sha: &mut Sha512, part: &[u8]| {
        sha.update((part.len() as u64).to_le_bytes());
        sha.update(part);
    };
    if is_alice {
        lp(&mut sha, my_name);
        lp(&mut sha, their_name);
        lp(&mut sha, &my.encoded);
        lp(&mut sha, their_msg);
    } else {
        lp(&mut sha, their_name);
        lp(&mut sha, my_name);
        lp(&mut sha, their_msg);
        lp(&mut sha, &my.encoded);
    }
    lp(&mut sha, &dh_shared_encoded);
    lp(&mut sha, &my.password_hash_64);

    let out = sha.finalize();
    let mut key = [0u8; 64];
    key.copy_from_slice(&out);
    Ok(key)
}

fn derive_aes_key(spake2_key_64: &[u8; 64]) -> [u8; 16] {
    let hk = Hkdf::<Sha256>::new(None, spake2_key_64);
    let mut okm = [0u8; 16];
    hk.expand(b"adb pairing_auth aes-128-gcm key", &mut okm)
        .expect("16 bytes is a valid HKDF-SHA256 output length");
    okm
}

struct AeadCipher {
    cipher: Aes128Gcm,
    enc_seq: u64,
    dec_seq: u64,
}

impl AeadCipher {
    fn new(aes_key: &[u8; 16]) -> Self {
        AeadCipher {
            cipher: Aes128Gcm::new_from_slice(aes_key).expect("16-byte key"),
            enc_seq: 0,
            dec_seq: 0,
        }
    }

    fn encrypt(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..8].copy_from_slice(&self.enc_seq.to_le_bytes());
        self.enc_seq += 1;
        self.cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
            .expect("encryption cannot fail with a valid key/nonce")
    }

    fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, String> {
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..8].copy_from_slice(&self.dec_seq.to_le_bytes());
        self.dec_seq += 1;
        self.cipher
            .decrypt(Nonce::from_slice(&nonce_bytes), ciphertext)
            .map_err(|_| "failed to decrypt peer info (wrong password or corrupted data)".to_string())
    }
}

// ---------- Packet framing (PairingPacketHeader: version, type, big-endian u32 len) ----------

const PACKET_VERSION: u8 = 1;
const PACKET_TYPE_SPAKE2_MSG: u8 = 0;
const PACKET_TYPE_PEER_INFO: u8 = 1;
const PEER_INFO_SIZE: usize = 8192;
const PEER_INFO_TYPE_RSA_PUB_KEY: u8 = 0;

fn write_packet(stream: &mut dyn Write, packet_type: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut header = [0u8; 6];
    header[0] = PACKET_VERSION;
    header[1] = packet_type;
    header[2..6].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    stream.write_all(&header)?;
    stream.write_all(payload)?;
    // Flush is essential: both sides write their packet then block reading the
    // other's. Without flushing, our record can sit in rustls' buffer and both
    // ends deadlock waiting on each other.
    stream.flush()
}

fn read_packet(stream: &mut dyn Read, expected_type: u8) -> Result<Vec<u8>, String> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header).map_err(|e| e.to_string())?;
    if header[0] != PACKET_VERSION {
        return Err(format!("unsupported pairing packet version {}", header[0]));
    }
    if header[1] != expected_type {
        return Err(format!(
            "unexpected pairing packet type (expected {}, got {})",
            expected_type, header[1]
        ));
    }
    let len = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    if len == 0 || len > PEER_INFO_SIZE * 2 {
        return Err(format!("pairing packet payload size out of range: {}", len));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;
    Ok(payload)
}

// ---------- Our persistent ADB identity ----------
// Normally adb generates ~/.android/adbkey the first time its server runs, but
// a machine that never used adb has none - start_qr_pairing calls
// crate::ensure_adb_key() (adb keygen) before we get here, so this read only
// fails if adb itself is missing/broken.

fn read_own_adb_pubkey() -> Result<String, String> {
    let path = crate::adb_pubkey_path()
        .ok_or_else(|| "could not determine the user's home directory".to_string())?;
    std::fs::read_to_string(&path)
        .map(|s| s.trim().to_string())
        .map_err(|e| format!("could not read {}: {}. Make sure adb is installed (it generates this key), then try again.", path.display(), e))
}

// ---------- TLS (self-signed cert, no peer-cert verification - matches adb's own
// SetCertVerifyCallback returning "always valid"; the real authentication is SPAKE2) ----------

// Only a server-cert verifier is needed - this app plays the TLS *client*
// role (the phone advertises the pairing service and runs the pairing
// server; the PC discovers it via mDNS and connects in). We accept any
// server certificate: adb's own pairing code does the same (its cert
// verify callback always returns "valid"); the real authentication is
// SPAKE2 with the QR password, not the TLS cert.
#[derive(Debug)]
struct AcceptAnyServerCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
        ]
    }
}

fn build_tls_client_config() -> Result<rustls::ClientConfig, String> {
    // We still present a self-signed client cert: Android's pairing server
    // requests one during the handshake. It isn't validated (SPAKE2 is the
    // real auth), but offering it keeps the mutual-TLS handshake happy.
    let cert_key = rcgen::generate_simple_self_signed(vec!["adb-pairing".to_string()])
        .map_err(|e| e.to_string())?;
    let cert_der = rustls::pki_types::CertificateDer::from(cert_key.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert_key.key_pair.serialize_der())
        .map_err(|e| e.to_string())?;

    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
        .with_client_auth_cert(vec![cert_der], key_der)
        .map_err(|e| e.to_string())?;
    config.key_log = Arc::new(rustls::NoKeyLog {});
    Ok(config)
}

// ---------- QR code rendering ----------

fn random_alnum(len: usize) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| CHARS[(rng.next_u32() as usize) % CHARS.len()] as char)
        .collect()
}

fn render_qr_data_url(text: &str) -> Result<String, String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|e| e.to_string())?;
    let image = code
        .render::<image::Luma<u8>>()
        .quiet_zone(true)
        .module_dimensions(8, 8)
        .build();
    let mut png_bytes: Vec<u8> = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &png_bytes)
    ))
}

// ---------- Shared session state (polled from the frontend, like the rest of this app) ----------

#[derive(Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PairingStatus {
    Idle,
    Waiting,
    Success {
        /// The phone's `_adb-tls-connect` address ("ip:port"), auto-discovered
        /// right after pairing so the UI can connect without asking the user
        /// to copy anything from the phone. None if discovery timed out.
        connect: Option<String>,
        /// The phone's hardware serial parsed from its `_adb-tls-connect` mDNS
        /// instance name (`adb-<serial>-<token>`). Used as the stable device
        /// identity so re-pairing the same phone doesn't create duplicates.
        /// None if not discovered/parseable.
        serial: Option<String>,
    },
    Error { message: String },
}

pub struct QrPairingState {
    inner: Mutex<PairingStatus>,
    generation: AtomicU64,
}

impl Default for QrPairingState {
    fn default() -> Self {
        QrPairingState {
            inner: Mutex::new(PairingStatus::Idle),
            generation: AtomicU64::new(0),
        }
    }
}

/// A fresh call to start_qr_pairing (or an explicit cancel) bumps the shared
/// generation counter; any in-flight session thread from a previous
/// generation notices the mismatch on its next poll and quietly stops
/// touching shared state, instead of needing a separate cancel flag/handle
/// to be threaded through and kept in sync.
fn run_pairing_session(
    state: Arc<QrPairingState>,
    mdns: mdns_sd::ServiceDaemon,
    receiver: mdns_sd::Receiver<mdns_sd::ServiceEvent>,
    service_name: String,
    password: Vec<u8>,
    generation: u64,
) {
    const TIMEOUT: Duration = Duration::from_secs(180);
    let start = std::time::Instant::now();

    // After the phone scans the QR it advertises
    // `<service_name>._adb-tls-pairing._tcp` pointing at *itself* and runs the
    // pairing server. We browse for that, match our exact service name, and
    // resolve it to the phone's address:port to connect to.
    let phone_addr: std::net::SocketAddr = loop {
        if state.generation.load(Ordering::Relaxed) != generation {
            let _ = mdns.shutdown();
            return; // superseded by a newer attempt or explicitly cancelled
        }
        if start.elapsed() > TIMEOUT {
            set_status(&state, generation, PairingStatus::Error {
                message: "Timed out waiting for the phone to scan the QR code.".to_string(),
            });
            let _ = mdns.shutdown();
            return;
        }
        match receiver.recv_timeout(Duration::from_millis(400)) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let full = info.get_fullname().to_string();
                let label = full.split('.').next().unwrap_or("");
                if label != service_name {
                    // Some other device's pairing service - ignore it.
                    log_line(&format!("ignoring unrelated pairing service '{}'", full));
                    continue;
                }
                let ipv4 = info.get_addresses_v4().into_iter().next();
                match ipv4 {
                    Some(ip) => {
                        let addr =
                            std::net::SocketAddr::new(std::net::IpAddr::V4(ip), info.get_port());
                        log_line(&format!("resolved phone pairing service {} at {}", full, addr));
                        break addr;
                    }
                    None => {
                        log_line(&format!("{} resolved but has no IPv4 address yet", full));
                        continue;
                    }
                }
            }
            Ok(_) => continue,
            Err(_) => continue, // timeout or transient - keep waiting until TIMEOUT
        }
    };

    let stream = match TcpStream::connect_timeout(&phone_addr, Duration::from_secs(10)) {
        Ok(s) => {
            log_line(&format!("connected to phone at {}", phone_addr));
            s
        }
        Err(e) => {
            log_line(&format!("could not connect to phone at {}: {}", phone_addr, e));
            set_status(&state, generation, PairingStatus::Error {
                message: format!("Found the phone but couldn't connect to it: {}", e),
            });
            let _ = mdns.shutdown();
            return;
        }
    };

    let result = run_pairing_protocol(stream, &password);
    match result {
        Ok(()) => {
            log_line("pairing protocol completed successfully");
            // Pairing only establishes trust; the phone still has to be
            // *connected* via its separate `_adb-tls-connect` service (whose
            // port differs from the pairing one and changes over time).
            // Discover it now, while we still have the mDNS daemon, so the UI
            // can run `adb connect` without asking the user for anything.
            let phone_ip = phone_addr.ip();
            let (connect, serial) = match discover_connect_addr(&mdns, phone_ip, Duration::from_secs(8)) {
                Some((addr, serial)) => {
                    log_line(&format!(
                        "discovered connect service at {}{}",
                        addr,
                        serial.as_deref().map(|s| format!(" (serial {})", s)).unwrap_or_default()
                    ));
                    (Some(addr), serial)
                }
                None => {
                    log_line("connect service not discovered within timeout (UI will ask for IP:Port)");
                    (None, None)
                }
            };
            set_status(&state, generation, PairingStatus::Success { connect, serial });
        }
        Err(msg) => {
            log_line(&format!("pairing protocol failed: {}", msg));
            set_status(&state, generation, PairingStatus::Error { message: msg });
        }
    }
    let _ = mdns.shutdown();
}

/// Extract the phone's hardware serial from an `_adb-tls-connect` mDNS instance
/// name of the form `adb-<serial>-<token>._adb-tls-connect._tcp.local.`,
/// e.g. `adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp.local.` -> `R5CX12ABCDE`.
/// The 6-char token after the last `-` is a per-session random suffix; Android
/// serials themselves don't contain `-`, so a right-split isolates the serial.
fn parse_adb_serial(fullname: &str) -> Option<String> {
    // Require the connect marker (split_once returns None if absent), so a
    // pairing-service name or anything else can't be mis-parsed into a serial.
    let label = fullname.split_once("._adb-tls-connect")?.0;
    let rest = label.strip_prefix("adb-")?;
    let (serial, _token) = rest.rsplit_once('-')?;
    if serial.is_empty() {
        None
    } else {
        Some(serial.to_string())
    }
}

/// Browse `_adb-tls-connect._tcp` and return ("ip:port", serial) for the service
/// belonging to `phone_ip` (the device we just paired with), ignoring any
/// other wireless-debugging phones on the network. The serial is None if the
/// instance name didn't parse.
fn discover_connect_addr(
    mdns: &mdns_sd::ServiceDaemon,
    phone_ip: std::net::IpAddr,
    timeout: Duration,
) -> Option<(String, Option<String>)> {
    let receiver = mdns.browse("_adb-tls-connect._tcp.local.").ok()?;
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match receiver.recv_timeout(Duration::from_millis(400)) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let matches_phone = info
                    .get_addresses_v4()
                    .into_iter()
                    .any(|a| std::net::IpAddr::V4(a) == phone_ip);
                if matches_phone {
                    let addr = format!("{}:{}", phone_ip, info.get_port());
                    return Some((addr, parse_adb_serial(info.get_fullname())));
                }
            }
            Ok(_) => {}
            Err(_) => {}
        }
    }
    None
}

/// Standalone mDNS lookup of a phone's CURRENT `_adb-tls-connect` ip:port.
/// Used by the reconnect path in main.rs to recover after the phone's
/// Wireless-debugging port changes. Uses mdns-sd (reliable) rather than
/// `adb mdns services`, which returns nothing in some environments (e.g. VMs).
///
/// With a `serial`, only announcements whose name parses to EXACTLY that serial
/// are candidates. Among them the one that also advertises the `ip` the PC
/// already had on file is preferred and returned as soon as it shows up; if it
/// has not shown up a fifth of the way into the window, the first serial-only
/// candidate is returned instead - the phone's IP changes via DHCP, refusing to
/// follow it would leave the reconnect stuck on a dead address, and making the
/// caller wait out the whole window for that is a needless stall. Without a
/// serial (a row saved without one), matching is by `ip` alone.
///
/// Neither rule is an identity check. The name is chosen by whoever advertises
/// it and the serial is broadcast in the clear, so anyone on the Wi-Fi can
/// publish a name that passes this filter; a stranger who also squats the old
/// IP passes the preference too. What this returns is where to DIAL. The
/// caller (`connect_and_grant`) confirms who answered over the adb channel
/// (`getprop ro.serialno`) before granting anything or matriculating.
pub fn discover_connect_for(serial: Option<&str>, ip: &str, timeout: Duration) -> Option<String> {
    let mdns = mdns_sd::ServiceDaemon::new().ok()?;
    let receiver = match mdns.browse("_adb-tls-connect._tcp.local.") {
        Ok(r) => r,
        Err(_) => {
            let _ = mdns.shutdown();
            return None;
        }
    };
    let start = std::time::Instant::now();
    let deadline = start + timeout;
    // Serial matched but on another IP: kept as the fallback.
    let mut moved: Option<String> = None;
    // ...but only held back for a FIFTH of the window (1 s at the single call
    // site's 5 s), not the whole of it: the exact-IP announcement is worth a
    // short wait, yet holding the fallback until the deadline would make every
    // reconnect after a DHCP address change cost the caller's full timeout.
    let moved_grace = start + timeout / 5;
    let mut found = None;
    while std::time::Instant::now() < deadline {
        if let Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) =
            receiver.recv_timeout(Duration::from_millis(400))
        {
            let addrs = info.get_addresses_v4();
            let port = info.get_port();
            match serial {
                // A non-matching serial is skipped with an `if`, not a
                // `continue`: continuing would jump over the grace check at
                // the bottom of the loop, so a Wi-Fi busy with other phones'
                // announcements would keep the fallback held to the deadline.
                Some(s) if parse_adb_serial(info.get_fullname()).as_deref() == Some(s) => {
                    if let Some(a) = addrs.iter().find(|a| a.to_string() == ip) {
                        found = Some(format!("{}:{}", a, port));
                        break;
                    }
                    if moved.is_none() {
                        if let Some(a) = addrs.into_iter().next() {
                            moved = Some(format!("{}:{}", a, port));
                        }
                    }
                }
                Some(_) => {}
                None => {
                    let ipv4 = addrs.into_iter().next();
                    if ipv4.map(|a| a.to_string()).as_deref() == Some(ip) {
                        if let Some(a) = ipv4 {
                            found = Some(format!("{}:{}", a, port));
                            break;
                        }
                    }
                }
            }
        }
        // Checked outside the recv branch so a quiet network releases the
        // fallback too, instead of waiting for another announcement.
        if moved.is_some() && std::time::Instant::now() >= moved_grace {
            break;
        }
    }
    let _ = mdns.shutdown();
    found.or(moved)
}

fn set_status(state: &Arc<QrPairingState>, generation: u64, status: PairingStatus) {
    if state.generation.load(Ordering::Relaxed) == generation {
        *state.inner.lock().unwrap() = status;
    }
}

// adb passes these to SPAKE2_CTX_new with `sizeof(kClientName)` as the length,
// which INCLUDES the trailing NUL (16 bytes, not 15). The names are hashed into
// the SPAKE2 transcript, so the NUL must be present or the derived key won't
// match the phone's. (Matches the same sizeof-with-NUL convention adb uses for
// the TLS exported-key label.)
const CLIENT_NAME: &[u8] = b"adb pair client\0";
const SERVER_NAME: &[u8] = b"adb pair server\0";
const EXPORTED_KEY_LABEL: &[u8] = b"adb-label\0"; // trailing NUL is significant, see tls_connection.cpp
const EXPORTED_KEY_SIZE: usize = 64;

fn run_pairing_protocol(mut stream: TcpStream, password: &[u8]) -> Result<(), String> {
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    stream.set_nodelay(true).ok();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    let tls_config = build_tls_client_config()?;
    // We accept any server cert, so the SNI name is irrelevant to auth; a fixed
    // placeholder keeps rustls happy.
    let server_name = rustls::pki_types::ServerName::try_from("adb-pairing")
        .map_err(|e| format!("invalid TLS server name: {}", e))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(tls_config), server_name)
        .map_err(|e| e.to_string())?;

    // Drive the handshake to completion explicitly before exporting keying
    // material - it must be fully done on both sides before either side is
    // allowed to derive/use the exported material (see SetupTlsConnection in
    // pairing_connection.cpp, which blocks on DoHandshake() first).
    while conn.is_handshaking() {
        conn.complete_io(&mut stream).map_err(|e| format!("TLS handshake failed: {}", e))?;
    }
    log_line(&format!("TLS handshake completed (version {:?})", conn.protocol_version()));

    let mut exported = [0u8; EXPORTED_KEY_SIZE];
    conn.export_keying_material(&mut exported, EXPORTED_KEY_LABEL, None)
        .map_err(|e| format!("TLS export_keying_material failed: {}", e))?;

    let mut tls_stream = rustls::Stream::new(&mut conn, &mut stream);

    // adb's pairing_connection.cpp appends the 64-byte TLS exported keying
    // material to the pairing password before SPAKE2 (confirmed against AOSP
    // source). This is channel binding against MITM.
    let mut effective_password = password.to_vec();
    effective_password.extend_from_slice(&exported);

    let m = decompress_point(M_POINT_HEX);
    let n = decompress_point(N_POINT_HEX);

    // We are the pairing *client* (alice) role: the phone runs the server, we
    // connect in after discovering it via mDNS.
    let my_msg = spake2_generate_msg(true, &effective_password, m, n);

    write_packet(&mut tls_stream, PACKET_TYPE_SPAKE2_MSG, &my_msg.encoded)
        .map_err(|e| e.to_string())?;
    log_line("sent our SPAKE2 message");
    let their_msg_bytes = read_packet(&mut tls_stream, PACKET_TYPE_SPAKE2_MSG)?;
    log_line(&format!("received peer SPAKE2 message ({} bytes)", their_msg_bytes.len()));
    if their_msg_bytes.len() != 32 {
        return Err("peer's SPAKE2 message had an unexpected size".to_string());
    }
    let mut their_msg = [0u8; 32];
    their_msg.copy_from_slice(&their_msg_bytes);

    let shared_key = spake2_process_msg(true, &their_msg, &my_msg, CLIENT_NAME, SERVER_NAME, m, n)?;
    let aes_key = derive_aes_key(&shared_key);
    let mut cipher = AeadCipher::new(&aes_key);

    let pubkey = read_own_adb_pubkey()?;
    let mut peer_info = vec![0u8; PEER_INFO_SIZE];
    peer_info[0] = PEER_INFO_TYPE_RSA_PUB_KEY;
    let key_bytes = pubkey.as_bytes();
    if key_bytes.len() > PEER_INFO_SIZE - 1 {
        return Err("local adb public key is unexpectedly large".to_string());
    }
    peer_info[1..1 + key_bytes.len()].copy_from_slice(key_bytes);

    let encrypted_out = cipher.encrypt(&peer_info);
    write_packet(&mut tls_stream, PACKET_TYPE_PEER_INFO, &encrypted_out).map_err(|e| e.to_string())?;
    log_line("sent our encrypted peer info (public key)");

    let encrypted_in = read_packet(&mut tls_stream, PACKET_TYPE_PEER_INFO)?;
    let decrypted = cipher.decrypt(&encrypted_in)?;
    log_line("decrypted peer info from phone");
    if decrypted.len() != PEER_INFO_SIZE {
        return Err("peer info had an unexpected size after decryption".to_string());
    }

    Ok(())
}

// ---------- Tauri commands ----------

#[tauri::command]
pub fn start_qr_pairing(
    state: tauri::State<'_, Arc<QrPairingState>>,
) -> Result<String, String> {
    // Bumping the generation counter is also how any previous in-flight
    // attempt gets superseded/ignored - see run_pairing_session.
    let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;

    // Fresh machines that never ran adb have no ~/.android/adbkey yet, and the
    // pairing protocol needs its public half as our peer info. Generate it now
    // (no-op if present) instead of failing later with "could not read
    // adbkey.pub". Failing HERE surfaces the problem in the UI before the QR is even shown
    // (e.g. adb not installed at all), instead of a cryptic error only after
    // the phone has scanned and the whole handshake ran.
    if let Err(e) = crate::ensure_adb_key() {
        log_line(&format!("adb key check failed: {}", e));
        return Err(e);
    }

    let service_name = format!("adbpair{}", random_alnum(10));
    let password = random_alnum(10);

    // The QR tells the phone which service name to advertise and the pairing
    // password. After scanning, the phone (NOT the PC) advertises
    // `<service_name>._adb-tls-pairing._tcp` pointing at itself and runs the
    // pairing server; we discover it below and connect in as the client.
    let qr_text = format!("WIFI:T:ADB;S:{};P:{};;", service_name, password);
    let qr_data_url = render_qr_data_url(&qr_text)?;

    // Browse for the phone's advertised pairing service. Deliberately no
    // advertising of our own: doing so makes Android see a name conflict while
    // it probes and rename its service (e.g. "<name> (2)"), so it would no
    // longer match the name in the QR and pairing could never complete.
    let mdns = mdns_sd::ServiceDaemon::new().map_err(|e| e.to_string())?;
    let receiver = mdns
        .browse("_adb-tls-pairing._tcp.local.")
        .map_err(|e| format!("could not start mDNS browse: {}", e))?;
    log_line(&format!(
        "start_qr_pairing: browsing for phone (service={}, gen={})",
        service_name, generation
    ));

    *state.inner.lock().unwrap() = PairingStatus::Waiting;

    let state_arc: Arc<QrPairingState> = state.inner().clone();
    let password_bytes = password.into_bytes();
    std::thread::spawn(move || {
        run_pairing_session(state_arc, mdns, receiver, service_name, password_bytes, generation);
    });

    Ok(qr_data_url)
}

#[tauri::command]
pub fn get_qr_pairing_status(state: tauri::State<'_, Arc<QrPairingState>>) -> PairingStatus {
    state.inner.lock().unwrap().clone()
}

#[tauri::command]
pub fn cancel_qr_pairing(state: tauri::State<'_, Arc<QrPairingState>>) {
    state.generation.fetch_add(1, Ordering::SeqCst);
    *state.inner.lock().unwrap() = PairingStatus::Idle;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known-answer test: fixed (non-random) ephemeral values and a fixed
    /// password. Guards the full client/server SPAKE2 path - message
    /// generation, the transcript hash (including the 16-byte NUL-terminated
    /// role names), the derived AES-128 key, and AES-GCM framing - against
    /// accidental changes. The expected values were captured from this same
    /// code once the whole flow was confirmed to pair successfully against a
    /// real phone (Android SPAKE2 = BoringSSL), so they anchor a
    /// known-interoperable state, not just internal self-consistency.
    #[test]
    fn spake2_matches_known_answer_vector() {
        let m = decompress_point(M_POINT_HEX);
        let n = decompress_point(N_POINT_HEX);

        assert_eq!(hex::encode(m.compress().to_bytes()), M_POINT_HEX);
        assert_eq!(hex::encode(n.compress().to_bytes()), N_POINT_HEX);

        let password = b"123456";
        let alice_random = [0x11u8; 64];
        let bob_random = [0x22u8; 64];

        // Use the EXACT production name constants (16 bytes each, trailing NUL
        // included) so this test exercises the same transcript the real pairing
        // does - without that NUL the derived key does not match the phone's.
        let alice_name = CLIENT_NAME;
        let bob_name = SERVER_NAME;

        let alice = spake2_generate_msg_with_random(true, password, &alice_random, m, n);
        let bob = spake2_generate_msg_with_random(false, password, &bob_random, m, n);

        // The outgoing messages depend only on password + ephemeral, NOT on the
        // names.
        assert_eq!(
            hex::encode(alice.encoded),
            "0217169d8949c1dc9fb329389e4ac43642ffe40e30c40937c80baf364c0bc837"
        );
        assert_eq!(
            hex::encode(bob.encoded),
            "711caacf0a733bad2eb43ee53459aee57f773c5cd93d83c2cbfe7ff056b4a28e"
        );

        let alice_key = spake2_process_msg(true, &bob.encoded, &alice, alice_name, bob_name, m, n).unwrap();
        let bob_key = spake2_process_msg(false, &alice.encoded, &bob, bob_name, alice_name, m, n).unwrap();

        assert_eq!(alice_key, bob_key);
        assert_eq!(
            hex::encode(alice_key),
            "755837a97b274f676d8347985013fb25452f642b041af4113ac990308910dc9ac3df62a775385f717e3cb29709017d59bdc61fa97e48c0845f367f508ff394cf"
        );

        let aes_key = derive_aes_key(&alice_key);
        assert_eq!(hex::encode(aes_key), "e3cf6abd3a757cb4a195fe222e7fda95");

        let mut enc_cipher = AeadCipher::new(&aes_key);
        let ct = enc_cipher.encrypt(b"hello peer info");
        assert_eq!(
            hex::encode(&ct),
            "3520da25906ebbca8a56d9a6c8b154d91dfe352e1844162b7db77320e6fa42"
        );

        let mut dec_cipher = AeadCipher::new(&aes_key);
        let pt = dec_cipher.decrypt(&ct).unwrap();
        assert_eq!(pt, b"hello peer info");
    }

    #[test]
    fn parses_adb_serial_from_connect_service_name() {
        assert_eq!(
            parse_adb_serial("adb-R5CX12ABCDE-62Sel0._adb-tls-connect._tcp.local."),
            Some("R5CX12ABCDE".to_string())
        );
        // Wrong service type / malformed names yield None, not a bad serial.
        assert_eq!(parse_adb_serial("adb-R5CX12ABCDE-62Sel0._adb-tls-pairing._tcp.local."), None);
        assert_eq!(parse_adb_serial("something-else.local."), None);
        assert_eq!(parse_adb_serial("adb-noTokenHere._adb-tls-connect._tcp.local."), None);
    }
}
