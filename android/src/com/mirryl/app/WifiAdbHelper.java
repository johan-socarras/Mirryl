package com.mirryl.app;

import android.app.AlarmManager;
import android.app.PendingIntent;
import android.app.job.JobInfo;
import android.app.job.JobScheduler;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkRequest;
import android.net.wifi.WifiManager;
import android.provider.Settings;
import android.text.format.Formatter;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.security.AlgorithmParameters;
import java.security.GeneralSecurityException;
import java.security.KeyFactory;
import java.security.MessageDigest;
import java.security.PublicKey;
import java.security.Signature;
import java.security.spec.ECFieldFp;
import java.security.spec.ECGenParameterSpec;
import java.security.spec.ECParameterSpec;
import java.security.spec.ECPoint;
import java.security.spec.ECPublicKeySpec;
import java.util.Arrays;
import java.util.Base64;
import java.util.UUID;
import org.json.JSONObject;

public final class WifiAdbHelper {

    private static final String ADB_WIFI_ENABLED = "adb_wifi_enabled";
    // Was 20 minutes. Android throttles setAndAllowWhileIdle/setExactAndAllowWhileIdle
    // to roughly once every 9 minutes per app in real deep Doze regardless of what's
    // requested here, so 10 minutes is close to the real floor without asking for
    // something the OS won't honor anyway.
    private static final long CHECK_INTERVAL_MS = 10 * 60 * 1000;
    private static final int ALARM_REQUEST_CODE = 1001;
    private static final int NETWORK_JOB_ID = 1002;
    /** The job armed by a change of the Wireless debugging switch: scheduleSwitchWatch. */
    static final int SWITCH_JOB_ID = 1003;
    /**
     * Brake on the switch watch. On a network the user never allowed Wireless
     * debugging on, Android turns the switch straight back off after we turn
     * it on (seen on the emulator: 1 written, 0 again within seconds). That
     * change re-triggers the switch watch, which would turn it on again - a
     * loop about once a second. So the switch watch writes at most once per
     * this window; the one-minute network job keeps trying at its own pace.
     */
    private static final long SWITCH_WRITE_GUARD_MS = 30 * 1000;
    private static final String WATCH_PREFS_NAME = "mirryl_watch";
    private static final String KEY_LAST_SWITCH_WRITE = "last_switch_write_ms";
    /**
     * The network connection (Network.getNetworkHandle) on which we last
     * turned the switch on and have not yet seen it stay on, and when.
     * See autoEnable.
     */
    private static final String KEY_PENDING_NETWORK = "pending_network";
    private static final String KEY_PENDING_AT = "pending_at_ms";
    /** The connection on which our switch was turned back off: no more automatic writes there. */
    private static final String KEY_REFUSED_NETWORK = "refused_network";
    /**
     * How long the switch has to stay on after our write before it counts as
     * accepted. On a network the user never allowed, Android briefly holds the
     * 1 we wrote before writing 0 (and the switch watch runs on that 1), so a
     * 1 seen right after our own write proves nothing.
     */
    private static final long ACCEPTED_AFTER_MS = 10 * 1000;
    /** See refusedHere. */
    private static final long REFUSED_WITHIN_MS = 2 * 60 * 1000;

    /**
     * Gate for {@link #enable}: a PC running Mirryl stamps this file
     * (Android/data/com.mirryl.app/files/.mstate, via adb push - no receiver,
     * no manifest change) on every connection it makes, so this app's own
     * keepalive can tell "a Mirryl PC touched me recently" from "no Mirryl PC
     * has ever connected". A phone that was never paired with Mirryl on a PC
     * (e.g. installed on its own) has no fresh stamp, and enable() below does
     * nothing.
     *
     * Honest limit: SEAL_PEPPER has to live in this APK to be checked here,
     * so anyone who decompiles it can forge a stamp by hand with plain adb -
     * this raises the bar, it is not a lock. The real binding is
     * {@link #isMatriculated}, signed per PC. MUST match SEAL_PEPPER in
     * main.rs and watcher.ps1's Send-Seal exactly - all three hash
     * independently.
     */
    private static final String SEAL_FILE_NAME = ".mstate";
    private static final String SEAL_PEPPER = "mirryl-seal-v1-8f2c";
    private static final long SEAL_MAX_AGE_SECS = 30L * 24 * 60 * 60;

    /**
     * Permanent replacement for the seal above - see {@link #isMatriculated}
     * for the full mechanism. Kept alongside SEAL_FILE_NAME/SEAL_PEPPER
     * rather than replacing them: {@link #enable} accepts either, so a phone
     * keeps working on the 30-day seal until a Mirryl PC matriculates it, at
     * which point it upgrades itself automatically.
     */
    private static final String DEVID_FILE_NAME = ".devid";
    private static final String MCERT_FILE_NAME = ".mcert";
    private static final String IDENTITY_PREFS_NAME = "mirryl_identity";
    private static final String KEY_DEVICE_ID = "device_id";
    private static final String KEY_TRUSTED_PC_PUBKEY = "trusted_pc_pubkey";
    /**
     * Remembers a foreign PC key the owner already said "not now" to, so
     * {@link #pendingPcReplacement} stops asking about that SAME key on
     * every refresh tick. A genuinely different foreign key later still
     * prompts again - this only silences repeats of one already-declined
     * request, it is not a second layer of trust.
     */
    private static final String KEY_DISMISSED_FOREIGN_PC = "dismissed_foreign_pc_pubkey";
    /**
     * This job is the mechanism that actually carries reconnection, so the
     * interval is what bounds how long the phone stays unreachable.
     *
     * A network callback cannot be the fast path - a PendingIntent network
     * request is one-shot, released seconds after it is created (see
     * scheduleSwitchWatch). This job stays at one minute as the backstop
     * behind the switch watch.
     */
    private static final long NETWORK_CHECK_LATENCY_MS = 60 * 1000;

    private WifiAdbHelper() {
    }

    /**
     * Turns on Android's "Wireless debugging" toggle (Settings.Global adb_wifi_enabled).
     * Requires the WRITE_SECURE_SETTINGS permission, granted once via:
     *   adb shell pm grant com.mirryl.app android.permission.WRITE_SECURE_SETTINGS
     * (Mirryl on the PC does this for you). That grant survives
     * reboots, so this call can run unattended from BootReceiver.
     *
     * Gated on {@link #hasFreshSeal} OR {@link #isMatriculated}: without
     * either a recent stamp or a permanent matriculation from a Mirryl
     * PC, this does nothing and returns false, same as the existing
     * SecurityException path below - a caller cannot tell any of these apart,
     * and does not need to.
     */
    public static boolean enable(Context context) {
        // The user asked (opened the app or pressed the button): forget any
        // refusal and try once more, even if Android asks again.
        watchPrefs(context).edit().remove(KEY_REFUSED_NETWORK).apply();
        return writeSwitch(context, activeNetworkHandle(context));
    }

    /**
     * enable() for the unattended paths (the jobs, the alarm, boot, an
     * update): the same write, except where the switch was already turned
     * back off on this very connection.
     *
     * On a network the user never marked "Always allow", turning the switch
     * on makes Android ask "Allow wireless debugging on this network?" and
     * write 0 at once (AdbDebuggingManager, MSG_ADBWIFI_ENABLE); "Deny" leaves
     * it at 0. Writing 1 again on every path would bring the question back
     * every minute and override a "Deny". So: if
     * the switch went off right after we turned it on, on the same connection
     * (refusedHere), that write was refused - by Android's question, by Deny, or by hand - and
     * nothing unattended writes again on that connection. A new connection
     * (back home, the WiFi dropped and returned) gets one new attempt; the
     * user allowing it ("Allow" writes 1) or opening the app clears it.
     *
     * Keyed on the connection, not the network name: reading the SSID or
     * BSSID needs location permission, and the gateway address repeats from
     * one house to the next. A connection the app cannot identify (handle 0)
     * is never treated as refused.
     */
    static boolean autoEnable(Context context) {
        SharedPreferences prefs = watchPrefs(context);
        if (isEnabled(context)) {
            long pendingAt = prefs.getLong(KEY_PENDING_AT, 0);
            long now = System.currentTimeMillis();
            SharedPreferences.Editor edit = prefs.edit().remove(KEY_REFUSED_NETWORK);
            if (now < pendingAt || now - pendingAt >= ACCEPTED_AFTER_MS) {
                edit.remove(KEY_PENDING_NETWORK).remove(KEY_PENDING_AT);
            }
            edit.apply();
            return true;
        }
        long network = activeNetworkHandle(context);
        if (refusedHere(context, network)) {
            return false;
        }
        return writeSwitch(context, network);
    }

    /**
     * Whether the switch, now off, was refused on this connection - and if it
     * has just been, records it. Only for an off switch.
     *
     * "Just" is REFUSED_WITHIN_MS after our write: Android answers at once,
     * so a switch that goes off later on the same connection went off for
     * another reason (a roam to another access point, Android's own WiFi
     * handling) and must not silence the app. The switch watch calls this
     * before its 30 s brake, so the refusal is seen within that window.
     */
    static boolean refusedHere(Context context, long network) {
        if (network == 0) {
            return false;
        }
        SharedPreferences prefs = watchPrefs(context);
        if (prefs.getLong(KEY_REFUSED_NETWORK, 0) == network) {
            return true;
        }
        long pendingAt = prefs.getLong(KEY_PENDING_AT, 0);
        long now = System.currentTimeMillis();
        if (prefs.getLong(KEY_PENDING_NETWORK, 0) == network
                && now >= pendingAt && now - pendingAt < REFUSED_WITHIN_MS) {
            prefs.edit()
                    .putLong(KEY_REFUSED_NETWORK, network)
                    .remove(KEY_PENDING_NETWORK)
                    .remove(KEY_PENDING_AT)
                    .apply();
            return true;
        }
        return false;
    }

    /** Network.getNetworkHandle() of the default network, or 0 when there is none. */
    static long activeNetworkHandle(Context context) {
        ConnectivityManager cm = (ConnectivityManager) context.getSystemService(Context.CONNECTIVITY_SERVICE);
        if (cm == null) {
            return 0;
        }
        Network network = cm.getActiveNetwork();
        return network == null ? 0 : network.getNetworkHandle();
    }

    private static SharedPreferences watchPrefs(Context context) {
        return context.getSharedPreferences(WATCH_PREFS_NAME, Context.MODE_PRIVATE);
    }

    private static boolean writeSwitch(Context context, long network) {
        // Published BEFORE the gate below: Java's && does not call
        // isMatriculated() while the seal is fresh, so a phone updated over a
        // live seal would never write the id the PC needs to matriculate it.
        // The PC pushes the seal first and reads .devid after.
        publishDeviceId(context);
        if (!hasFreshSeal(context) && !isMatriculated(context)) {
            return false;
        }
        try {
            Settings.Global.putInt(context.getContentResolver(), ADB_WIFI_ENABLED, 1);
            // Only after a write that went through: without the permission
            // nothing was written, and nothing could have been refused.
            if (network != 0) {
                watchPrefs(context).edit()
                        .putLong(KEY_PENDING_NETWORK, network)
                        .putLong(KEY_PENDING_AT, System.currentTimeMillis())
                        .apply();
            }
            return isEnabled(context);
        } catch (SecurityException e) {
            return false;
        }
    }

    /**
     * True when Android/data/com.mirryl.app/files/.mstate exists, parses, and
     * is younger than SEAL_MAX_AGE_SECS - see the field javadoc above for what
     * this proves and does not prove.
     */
    static boolean hasFreshSeal(Context context) {
        File dir = context.getExternalFilesDir(null);
        if (dir == null) {
            return false;
        }
        File file = new File(dir, SEAL_FILE_NAME);
        if (!file.exists()) {
            return false;
        }
        try {
            String content = readFile(file);
            JSONObject obj = new JSONObject(content);
            String value = obj.getString("v");
            long ts = obj.getLong("ts");
            long nowSecs = System.currentTimeMillis() / 1000L;
            // Reject a future-dated stamp too, not just a stale one - a clock
            // set forward should not buy a longer window than a real PC ever
            // grants.
            if (ts > nowSecs + 3600 || nowSecs - ts > SEAL_MAX_AGE_SECS) {
                return false;
            }
            return sha256Hex(SEAL_PEPPER + ":" + ts).equals(value);
        } catch (Exception e) {
            return false;
        }
    }

    private static String readFile(File file) throws Exception {
        try (FileInputStream in = new FileInputStream(file)) {
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[256];
            int n;
            while ((n = in.read(buf)) != -1) {
                out.write(buf, 0, n);
            }
            return out.toString("UTF-8");
        }
    }

    private static String sha256Hex(String input) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        byte[] hash = digest.digest(input.getBytes(StandardCharsets.UTF_8));
        StringBuilder sb = new StringBuilder(hash.length * 2);
        for (byte b : hash) {
            sb.append(String.format("%02x", b));
        }
        return sb.toString();
    }

    /**
     * This device's own permanent identity: a random id generated once and
     * never changed after that. Lives in SharedPreferences, which is this
     * app's PRIVATE storage - `adb shell` (the `shell` user) cannot read or
     * write another app's private storage without root, which is the actual
     * wall the whole matriculation design leans on. See {@link
     * #isMatriculated} for why that matters.
     */
    private static String getOrCreateDeviceId(Context context) {
        SharedPreferences prefs = context.getSharedPreferences(IDENTITY_PREFS_NAME, Context.MODE_PRIVATE);
        String id = prefs.getString(KEY_DEVICE_ID, null);
        if (id == null) {
            id = UUID.randomUUID().toString();
            prefs.edit().putString(KEY_DEVICE_ID, id).apply();
        }
        return id;
    }

    /**
     * Publishes a plain-text copy of the private device id to external
     * storage, so a PC can read it with `adb pull`/`cat` while matriculating.
     * Write-only from this app's own point of view: {@link #isMatriculated}
     * always compares against the private copy above, never against a
     * re-read of this file - anyone with adb access can edit this external
     * copy freely, so it must never be trusted as this device's identity,
     * only published as it.
     */
    /**
     * Makes sure the mailbox copy of this device's id exists, whatever the
     * seal or the matriculation say. Cheap (one small file write), safe to
     * call on every start-up and every toggle.
     */
    static void publishDeviceId(Context context) {
        exportDeviceId(context, getOrCreateDeviceId(context));
    }

    private static void exportDeviceId(Context context, String deviceId) {
        File dir = context.getExternalFilesDir(null);
        if (dir == null) {
            return;
        }
        try (FileOutputStream out = new FileOutputStream(new File(dir, DEVID_FILE_NAME))) {
            out.write(deviceId.getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            // Best-effort mailbox update - a failed write here just means a
            // PC connecting right now won't see a fresh copy, not that
            // anything is broken; the next successful call fixes it.
        }
    }

    /**
     * Permanent replacement for {@link #hasFreshSeal} (the PC half is
     * matriculation.rs in the PC app). A PC signs a
     * `.mcert` with its own locally-generated P-256 key (never a constant
     * shared across every install, unlike SEAL_PEPPER). The FIRST valid
     * `.mcert` this device ever sees PINS that PC's public key into private
     * storage, permanently; every check after that only accepts a `.mcert`
     * signed by that SAME pinned key, ignoring any other - even a perfectly
     * self-consistent one from a different PC.
     *
     * That pin living in private storage, written exactly once, is what
     * makes this a real lock rather than a suggestion: external storage is a
     * mailbox anyone with adb access can write to, so the one thing that
     * must never be read from there is the decision of who is already
     * trusted. Moving a phone to a different PC means reinstalling this app,
     * which clears the pin.
     */
    static boolean isMatriculated(Context context) {
        String deviceId = getOrCreateDeviceId(context);
        exportDeviceId(context, deviceId);

        McertFields cert = readMcert(context);
        if (cert == null || !deviceId.equals(cert.dev)) {
            return false;
        }

        SharedPreferences prefs = context.getSharedPreferences(IDENTITY_PREFS_NAME, Context.MODE_PRIVATE);
        String trustedPc = prefs.getString(KEY_TRUSTED_PC_PUBKEY, null);

        if (trustedPc == null) {
            if (!verifyMcertSignature(cert.dev, cert.pc, cert.sig)) {
                return false;
            }
            // Pin, once, forever. Must never be overwritten after this -
            // see the class javadoc above for what breaks if it is.
            prefs.edit().putString(KEY_TRUSTED_PC_PUBKEY, cert.pc).apply();
            return true;
        }

        return trustedPc.equals(cert.pc) && verifyMcertSignature(cert.dev, cert.pc, cert.sig);
    }

    /**
     * Public key of a PC whose `.mcert` is well-formed, signed correctly and
     * addressed to this device, but does NOT match the already-pinned
     * trusted PC - or null if there is nothing like that waiting. This is
     * the one and only door into replacing a pin, and it is deliberately
     * NOT automatic: {@link #isMatriculated} never calls this and never
     * accepts a foreign cert on its own, no matter how valid it looks.
     * MainActivity uses this to ask the owner directly ("a different
     * computer wants to connect - is this your new one?") instead of either
     * locking them out forever or silently trusting whichever PC last
     * touched external storage.
     *
     * Returns null (asks nothing) once the owner has already said "not now"
     * to this SAME key - see {@link #dismissPcReplacement}. A genuinely
     * different foreign key later still prompts again.
     */
    static String pendingPcReplacement(Context context) {
        String deviceId = getOrCreateDeviceId(context);
        McertFields cert = readMcert(context);
        if (cert == null || !deviceId.equals(cert.dev)) {
            return null;
        }

        SharedPreferences prefs = context.getSharedPreferences(IDENTITY_PREFS_NAME, Context.MODE_PRIVATE);
        String trustedPc = prefs.getString(KEY_TRUSTED_PC_PUBKEY, null);
        if (trustedPc == null || trustedPc.equals(cert.pc)) {
            // Nothing pinned yet (isMatriculated's own first-pin path
            // handles that), or this IS the already-trusted PC - neither is
            // a "replacement" to ask about.
            return null;
        }
        if (cert.pc.equals(prefs.getString(KEY_DISMISSED_FOREIGN_PC, null))) {
            return null;
        }
        if (!verifyMcertSignature(cert.dev, cert.pc, cert.sig)) {
            // Never offer to trust a cert that does not even check out
            // cryptographically - only a genuinely valid one from a
            // genuinely different PC is a real replacement candidate.
            return null;
        }
        return cert.pc;
    }

    /** Owner confirmed the prompt from {@link #pendingPcReplacement}: re-pin to the new PC. */
    static void acceptPcReplacement(Context context, String newPc) {
        context.getSharedPreferences(IDENTITY_PREFS_NAME, Context.MODE_PRIVATE)
                .edit()
                .putString(KEY_TRUSTED_PC_PUBKEY, newPc)
                .remove(KEY_DISMISSED_FOREIGN_PC)
                .apply();
    }

    /** Owner dismissed the prompt from {@link #pendingPcReplacement}: stop asking about this key. */
    static void dismissPcReplacement(Context context, String pc) {
        context.getSharedPreferences(IDENTITY_PREFS_NAME, Context.MODE_PRIVATE)
                .edit()
                .putString(KEY_DISMISSED_FOREIGN_PC, pc)
                .apply();
    }

    /** Parsed `.mcert` fields, or null if the file is missing or malformed. */
    private static McertFields readMcert(Context context) {
        File dir = context.getExternalFilesDir(null);
        if (dir == null) {
            return null;
        }
        File file = new File(dir, MCERT_FILE_NAME);
        if (!file.exists()) {
            return null;
        }
        try {
            JSONObject obj = new JSONObject(readFile(file));
            if (obj.getInt("v") != 2) {
                return null;
            }
            return new McertFields(obj.getString("dev"), obj.getString("pc"), obj.getString("sig"));
        } catch (Exception e) {
            return null;
        }
    }

    private static final class McertFields {
        final String dev;
        final String pc;
        final String sig;

        McertFields(String dev, String pc, String sig) {
            this.dev = dev;
            this.pc = pc;
            this.sig = sig;
        }
    }

    /**
     * Verifies an ECDSA-P256-SHA256 signature over "dev:pc" (UTF-8 bytes,
     * exactly as the PC side signs it in matriculation.rs) under the P-256
     * public key encoded in `pc`. MUST match the PC's wire format exactly:
     * `pc` is a base64 SEC1-compressed point (33 bytes) and `sig` is a
     * base64 DER-encoded signature - the full format is described at the top
     * of matriculation.rs in the PC app. P-256 (not Ed25519) was chosen
     * because it verifies with the platform's built-in
     * "EC"/"SHA256withECDSA" providers, no new dependency needed.
     */
    private static boolean verifyMcertSignature(String dev, String pcPubKeyB64, String sigB64) {
        try {
            byte[] pcBytes = Base64.getDecoder().decode(pcPubKeyB64);
            byte[] sigBytes = Base64.getDecoder().decode(sigB64);
            PublicKey publicKey = decodeP256PublicKey(pcBytes);
            Signature signature = Signature.getInstance("SHA256withECDSA");
            signature.initVerify(publicKey);
            signature.update((dev + ":" + pcPubKeyB64).getBytes(StandardCharsets.UTF_8));
            return signature.verify(sigBytes);
        } catch (Exception e) {
            return false;
        }
    }

    /**
     * Reconstructs a P-256 public key from a SEC1-compressed point (0x02/0x03
     * prefix + 32-byte X coordinate) using only java.security - no Bouncy
     * Castle or other new dependency. Decompression solves the curve
     * equation y^2 = x^3 + a*x + b (mod p) for y, then picks the root whose
     * parity matches the prefix byte. The modPow shortcut for the square
     * root only holds because P-256's prime is congruent to 3 (mod 4), which
     * it is - this is not a general-purpose decompressor.
     */
    private static PublicKey decodeP256PublicKey(byte[] compressed) throws GeneralSecurityException {
        if (compressed.length != 33 || (compressed[0] != 0x02 && compressed[0] != 0x03)) {
            throw new java.security.spec.InvalidKeySpecException("not a compressed P-256 point");
        }
        AlgorithmParameters params = AlgorithmParameters.getInstance("EC");
        params.init(new ECGenParameterSpec("secp256r1"));
        ECParameterSpec ecSpec = params.getParameterSpec(ECParameterSpec.class);

        BigInteger p = ((ECFieldFp) ecSpec.getCurve().getField()).getP();
        BigInteger a = ecSpec.getCurve().getA();
        BigInteger b = ecSpec.getCurve().getB();

        BigInteger x = new BigInteger(1, Arrays.copyOfRange(compressed, 1, 33));
        BigInteger alpha = x.modPow(BigInteger.valueOf(3), p).add(a.multiply(x)).add(b).mod(p);
        BigInteger y = alpha.modPow(p.add(BigInteger.ONE).shiftRight(2), p);
        boolean wantOdd = compressed[0] == 0x03;
        if (y.testBit(0) != wantOdd) {
            y = p.subtract(y);
        }

        KeyFactory factory = KeyFactory.getInstance("EC");
        return factory.generatePublic(new ECPublicKeySpec(new ECPoint(x, y), ecSpec));
    }

    public static boolean isEnabled(Context context) {
        return Settings.Global.getInt(context.getContentResolver(), ADB_WIFI_ENABLED, 0) == 1;
    }

    /**
     * Live check for whether WRITE_SECURE_SETTINGS actually works right now, by
     * writing the current value back to itself (a no-op) and seeing if it
     * throws. Deliberately NOT using checkSelfPermission()/PackageManager here:
     * that check can be served from an in-process permission cache that only
     * gets invalidated for normal runtime-permission grant/revoke calls, so it
     * can keep reporting "granted" for a while after WRITE_SECURE_SETTINGS is
     * revoked out-of-process (Settings UI, `pm revoke`) until this app's
     * process is restarted. Writing to SettingsProvider is enforced live on
     * every call, so it reflects the real current state.
     */
    public static boolean hasWriteSecureSettingsPermission(Context context) {
        try {
            int current = Settings.Global.getInt(context.getContentResolver(), ADB_WIFI_ENABLED, 0);
            Settings.Global.putInt(context.getContentResolver(), ADB_WIFI_ENABLED, current);
            return true;
        } catch (SecurityException e) {
            return false;
        }
    }

    /**
     * True if this app is on Android's Doze/App Standby exemption whitelist.
     * When it isn't, the background reconnect paths (the alarm in
     * scheduleNextCheck, the job in NetworkJobService) are throttled while
     * the device is idle - measured directly forcing deep idle (adb shell
     * dumpsys deviceidle force-idle) with this OFF: reconnection blocked for
     * 3+ minutes straight, recovered in 5 seconds the instant idle lifted.
     *
     * Measured on a Samsung, awake: without it the app sits in the "rare"
     * standby bucket, its alarm is deferred by app standby for 356 days and
     * wireless debugging does not come back after a WiFi drop on its own;
     * with it (bucket 5, EXEMPTED) the job brings it back in 4 min.
     *
     * The app does not ask for this itself (the manifest declares no
     * REQUEST_IGNORE_BATTERY_OPTIMIZATIONS): Mirryl
     * on the PC adds the app to the whitelist over adb when it connects, the
     * same way it grants WRITE_SECURE_SETTINGS. This only reads the result.
     */
    public static boolean isIgnoringBatteryOptimizations(Context context) {
        android.os.PowerManager pm =
                (android.os.PowerManager) context.getSystemService(Context.POWER_SERVICE);
        return pm != null && pm.isIgnoringBatteryOptimizations(context.getPackageName());
    }

    /**
     * Opens this app's own "App info" system settings screen, so the user can
     * navigate to Battery and turn the exemption back off themselves. App
     * info -> Battery is the standard, always-available path on stock Android
     * and every OEM skin checked for this project. Needs no permission.
     */
    public static void openAppBatterySettings(android.app.Activity activity) {
        Intent intent = new Intent(android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS);
        intent.setData(android.net.Uri.parse("package:" + activity.getPackageName()));
        try {
            activity.startActivity(intent);
        } catch (android.content.ActivityNotFoundException e) {
            // Practically unreachable - this screen has existed since API 9.
        }
    }

    /**
     * Schedules a one-shot inexact alarm ~10 min from now that re-checks and
     * re-enables wireless debugging, then reschedules itself. Acts as a safety
     * net in case something other than a reboot (OEM battery manager, WiFi
     * reconnect, etc.) turns the toggle back off. Uses setAndAllowWhileIdle
     * (not the Exact variant) so it needs no extra runtime permission.
     */
    public static void scheduleNextCheck(Context context) {
        AlarmManager alarmManager = (AlarmManager) context.getSystemService(Context.ALARM_SERVICE);
        if (alarmManager == null) {
            return;
        }
        Intent intent = new Intent(context, AlarmReceiver.class);
        PendingIntent pendingIntent = PendingIntent.getBroadcast(
                context, ALARM_REQUEST_CODE, intent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        long triggerAt = System.currentTimeMillis() + CHECK_INTERVAL_MS;
        alarmManager.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, triggerAt, pendingIntent);
    }

    /**
     * Schedules the job that re-enables wireless debugging as soon as the
     * device has a network again. The 10-minute alarm above only bounds how
     * long the phone can stay unreachable; this is what makes recovery from a
     * WiFi toggle take about a minute instead.
     *
     * A manifest receiver cannot do this: CONNECTIVITY_ACTION stopped being
     * deliverable to manifest receivers in Android 7, and this app has no
     * long-lived process to register a NetworkCallback from. A persisted job
     * with a network constraint is the supported way to be woken when
     * connectivity returns, and it needs no foreground service or notification.
     *
     * One-shot rather than periodic on purpose: setMinimumLatency cannot be
     * combined with setPeriodic, and that latency is what keeps the job from
     * firing back-to-back while the network is simply up. NetworkJobService
     * re-arms it on every run, so a job is always pending for the next time
     * connectivity returns. Scheduling an id that is already scheduled replaces
     * it, so calling this more than once is harmless.
     */
    public static void scheduleNetworkCheck(Context context) {
        JobScheduler scheduler = (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler == null) {
            return;
        }
        // NOT setRequiredNetworkType(NETWORK_TYPE_ANY): that expands to a request
        // for INTERNET *and* VALIDATED capabilities, so on a WiFi network with no
        // working internet - or during the seconds Android spends validating one
        // - the job stays blocked. Wireless debugging only needs the LAN, so the
        // requirement here is simply "there is a network", built from a bare
        // NetworkRequest that adds neither capability.
        JobInfo job = new JobInfo.Builder(
                NETWORK_JOB_ID,
                new ComponentName(context, NetworkJobService.class))
                .setRequiredNetwork(new NetworkRequest.Builder().build())
                .setMinimumLatency(NETWORK_CHECK_LATENCY_MS)
                .setPersisted(true)
                .build();
        scheduler.schedule(job);
    }

    /**
     * Whether the switch watch may turn the switch on now, and if so records
     * that it is about to - see SWITCH_WRITE_GUARD_MS. A clock that moved
     * backwards counts as "long ago", so the brake can never stick.
     */
    static boolean claimSwitchWrite(Context context) {
        SharedPreferences prefs = context.getSharedPreferences(WATCH_PREFS_NAME, Context.MODE_PRIVATE);
        long now = System.currentTimeMillis();
        long last = prefs.getLong(KEY_LAST_SWITCH_WRITE, 0);
        if (now >= last && now - last < SWITCH_WRITE_GUARD_MS) {
            return false;
        }
        prefs.edit().putLong(KEY_LAST_SWITCH_WRITE, now).apply();
        return true;
    }

    /** scheduleSwitchWatch, but only when it is not already pending. */
    static void ensureSwitchWatch(Context context) {
        JobScheduler scheduler = (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler != null && scheduler.getPendingJob(SWITCH_JOB_ID) == null) {
            scheduleSwitchWatch(context);
        }
    }

    /**
     * The fast path back after a WiFi drop: a job JobScheduler starts when the
     * Wireless debugging switch (Settings.Global adb_wifi_enabled) CHANGES and
     * a network is available. When WiFi drops, Android itself writes 0 there
     * ("Network disconnected. Disabling adbwifi."); that change arms this job,
     * and it runs the moment a network is back, which is exactly when the
     * switch can be turned on again.
     *
     * Not registerNetworkCallback(NetworkRequest, PendingIntent), which does
     * not work for this: a PendingIntent network request is ONE-SHOT. The
     * platform fires it once, for the first network that matches, and releases
     * it 5 s after delivery (ConnectivityService.sendPendingIntentForRequest /
     * releasePendingNetworkRequestWithDelay). Registered with WiFi already up,
     * it fires at once for that network and is gone long before WiFi actually
     * drops.
     *
     * A content-triggered job is re-armed by the app, not consumed by the
     * platform: NetworkJobService reschedules it on every run. Our own write of
     * 1 also changes the URI and runs the job once more, which finds the switch
     * on and does nothing else. It cannot be persisted (JobInfo throws for
     * content triggers on persisted jobs), so BootReceiver re-arms it after a
     * reboot, like the alarm.
     */
    public static void scheduleSwitchWatch(Context context) {
        JobScheduler scheduler = (JobScheduler) context.getSystemService(Context.JOB_SCHEDULER_SERVICE);
        if (scheduler == null) {
            return;
        }
        JobInfo job = new JobInfo.Builder(
                SWITCH_JOB_ID,
                new ComponentName(context, NetworkJobService.class))
                .addTriggerContentUri(new JobInfo.TriggerContentUri(
                        Settings.Global.getUriFor(ADB_WIFI_ENABLED), 0))
                // 0 is raised by the platform to its own minimum; the default
                // would wait 10 s after the change for no reason here.
                .setTriggerContentUpdateDelay(0)
                .setTriggerContentMaxDelay(0)
                // Same bare request as scheduleNetworkCheck: the LAN is enough.
                .setRequiredNetwork(new NetworkRequest.Builder().build())
                .build();
        try {
            scheduler.schedule(job);
        } catch (RuntimeException ignored) {
            // An OEM refusing content triggers must not take the app down:
            // the network job and the alarm still cover it, only slower.
        }
    }

    public static String currentIp(Context context) {
        WifiManager wifiManager = (WifiManager) context.getApplicationContext()
                .getSystemService(Context.WIFI_SERVICE);
        if (wifiManager == null) {
            return "unknown";
        }
        int ipInt = wifiManager.getConnectionInfo().getIpAddress();
        if (ipInt == 0) {
            return "no WiFi connection";
        }
        return Formatter.formatIpAddress(ipInt);
    }
}
