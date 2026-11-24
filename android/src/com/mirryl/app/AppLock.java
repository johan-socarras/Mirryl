package com.mirryl.app;

import android.content.Context;
import android.content.SharedPreferences;
import android.os.SystemClock;
import android.util.Base64;

import java.security.MessageDigest;
import java.security.SecureRandom;
import java.util.HashSet;
import java.util.Set;

import javax.crypto.SecretKeyFactory;
import javax.crypto.spec.PBEKeySpec;

/**
 * The apps protected with a PIN on this phone, and the PIN itself.
 *
 * Mirryl on the PC sets both (LockConfigReceiver); AppLockService asks
 * {@link #mustAsk} each time an app comes to the front, and LockActivity
 * checks what the person types with {@link #checkPin}. The PIN is never kept:
 * only a salted PBKDF2 hash, in this app's private storage.
 */
final class AppLock {
    private static final String PREFS = "app_lock";
    private static final String KEY_LOCKED = "locked";
    private static final String KEY_HASH = "pin_hash";
    private static final String KEY_SALT = "pin_salt";
    private static final int ITERATIONS = 20000;
    /** After this many wrong PINs in a row, the next try waits. */
    private static final int FREE_TRIES = 5;
    private static final long WAIT_MS = 30000;

    /** The app opened with the right PIN; it stays open until another app comes to the front. */
    private static String unlockedPackage = null;
    private static int wrongTries = 0;
    private static long waitUntil = 0;

    private AppLock() {
    }

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    static Set<String> locked(Context context) {
        return new HashSet<String>(prefs(context).getStringSet(KEY_LOCKED, new HashSet<String>()));
    }

    static void setLocked(Context context, String pkg, boolean on) {
        Set<String> set = locked(context);
        if (on) {
            set.add(pkg);
        } else {
            set.remove(pkg);
        }
        prefs(context).edit().putStringSet(KEY_LOCKED, set).apply();
        if (!on && pkg.equals(unlockedPackage)) {
            unlockedPackage = null;
        }
    }

    static boolean hasPin(Context context) {
        return prefs(context).getString(KEY_HASH, null) != null;
    }

    static void setPin(Context context, String pin) {
        byte[] salt = new byte[16];
        new SecureRandom().nextBytes(salt);
        prefs(context).edit()
                .putString(KEY_SALT, Base64.encodeToString(salt, Base64.NO_WRAP))
                .putString(KEY_HASH, Base64.encodeToString(hash(pin, salt), Base64.NO_WRAP))
                .apply();
        wrongTries = 0;
        waitUntil = 0;
    }

    /** Seconds left before another try is allowed, 0 when one is. */
    static long secondsToWait() {
        long left = waitUntil - SystemClock.elapsedRealtime();
        return left > 0 ? (left + 999) / 1000 : 0;
    }

    static boolean checkPin(Context context, String pin) {
        // Nothing typed is not a try, and PBKDF2 refuses an empty password.
        if (pin == null || pin.isEmpty() || secondsToWait() > 0) {
            return false;
        }
        SharedPreferences p = prefs(context);
        String storedHash = p.getString(KEY_HASH, null);
        String storedSalt = p.getString(KEY_SALT, null);
        if (storedHash == null || storedSalt == null) {
            return false;
        }
        boolean ok;
        try {
            byte[] expected = Base64.decode(storedHash, Base64.NO_WRAP);
            byte[] actual = hash(pin, Base64.decode(storedSalt, Base64.NO_WRAP));
            ok = MessageDigest.isEqual(expected, actual);
        } catch (RuntimeException error) {
            // A check that cannot be done is a wrong PIN, never an open door.
            ok = false;
        }
        if (ok) {
            wrongTries = 0;
        } else if (++wrongTries >= FREE_TRIES) {
            waitUntil = SystemClock.elapsedRealtime() + WAIT_MS;
        }
        return ok;
    }

    private static byte[] hash(String pin, byte[] salt) {
        try {
            PBEKeySpec spec = new PBEKeySpec(pin.toCharArray(), salt, ITERATIONS, 256);
            return SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256").generateSecret(spec).getEncoded();
        } catch (Exception error) {
            throw new IllegalStateException(error);
        }
    }

    /**
     * Apps that are part of another one: Settings' search is its own package,
     * and protecting Settings without it would leave its search open.
     */
    private static String owner(String pkg) {
        if ("com.google.android.settings.intelligence".equals(pkg)
                || "com.android.settings.intelligence".equals(pkg)) {
            return "com.android.settings";
        }
        return pkg;
    }

    /** Whether the app now in front has to ask for the PIN. */
    static boolean mustAsk(Context context, String pkg) {
        String app = owner(pkg);
        return hasPin(context) && locked(context).contains(app) && !app.equals(unlockedPackage);
    }

    /** The package whose PIN opens `pkg` (itself, or the app it belongs to). */
    static String ownerOf(String pkg) {
        return owner(pkg);
    }

    static void unlocked(String pkg) {
        unlockedPackage = pkg;
    }

    /** Another app came to the front, or the screen went off: the next visit asks again. */
    static void relock() {
        unlockedPackage = null;
    }

    static String unlockedPackage() {
        return unlockedPackage;
    }
}
