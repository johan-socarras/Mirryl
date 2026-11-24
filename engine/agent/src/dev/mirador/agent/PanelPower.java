package dev.mirador.agent;

import android.content.Context;
import android.media.AudioManager;
import android.os.IBinder;
import android.os.PowerManager;
import android.os.SystemClock;
import android.util.Log;

import java.lang.reflect.Method;

/**
 * Turns the phone's physical panel off while the compositor keeps running, so
 * the mirror stays live on a screen that looks switched off.
 *
 * Measured on a Samsung (Android 16): with the panel off the same
 * full-screen animation still arrives at 38.6 painted
 * frames a second against 40.9 with the panel on, and nothing freezes. The
 * feature is worth having because the picture does NOT stop.
 *
 * Two things about this are deliberate and easy to get wrong later:
 *
 * 1. It NEVER forces the panel back to normal blindly. Restoring means setting
 *    the mode that matches what Android itself believes the display is doing
 *    right now. Blindly setting NORMAL would light up a phone that had gone to
 *    sleep in someone's pocket - the exact class of surprise this feature has
 *    to avoid.
 *
 * 2. It RELEASES ITSELF the moment Android's own display state moves. Android
 *    does not know the panel is off - it keeps reporting STATE_ON the whole
 *    time, which was verified on the device - so a state change means the
 *    phone's real power state moved for some other reason: it dozed on its
 *    timeout, or somebody pressed the power button. Fighting that would mean
 *    blanking the screen of a person who just picked their phone up.
 */
final class PanelPower {
    private static final String LOG_TAG = "MirrylPanel";

    /** SurfaceControl power modes. */
    private static final int POWER_MODE_OFF = 0;
    private static final int POWER_MODE_DOZE = 1;
    private static final int POWER_MODE_NORMAL = 2;
    private static final int POWER_MODE_DOZE_SUSPEND = 3;

    /** android.view.Display states, which do NOT share the numbering above. */
    private static final int STATE_OFF = 1;
    private static final int STATE_ON = 2;
    private static final int STATE_DOZE = 3;
    private static final int STATE_DOZE_SUSPEND = 4;

    /** Same cadence as the rotation watcher: cheap next to encoding video. */
    private static final long WATCH_POLL_MS = 400L;

    /**
     * How long the PC may go quiet before the panel is given back regardless.
     *
     * This is the safety net. Giving the panel back cannot depend on this
     * agent's socket reporting end-of-file when the PC dies - which it does
     * for a process that exits, and does NOT do for Wi-Fi that vanishes: a TCP
     * connection whose other end stopped answering can sit there for minutes
     * before the kernel gives up, the phone holding a dark screen the whole
     * time. A thread parked in a blocking read on the PC side is enough to
     * keep the socket from closing promptly, and a safety property that a
     * distant change can break by accident is not a safety property.
     *
     * So the PC has to keep SAYING it is there, and silence is enough. Three
     * seconds against a ping every one, so a couple of lost or late messages
     * are not mistaken for a dead PC.
     */
    private static final long HOST_SILENCE_MS = 3000L;

    private static volatile boolean engaged;
    private static volatile long lastHostContactMs;
    private static volatile AudioManager audioManager;
    private static volatile PowerManager.WakeLock wakeLock;

    /** Told about every change, so the PC can show what the phone is doing. */
    interface Listener {
        void onPanelStateChanged(boolean off);
    }

    private static volatile Listener listener;

    private PanelPower() {
    }

    /**
     * Gives this class the Context it needs to reach AudioManager. Called once
     * from ControlStreamMain, which already builds a shell Context to reach
     * InputManager the same way.
     *
     * Media is muted for the same reason the panel goes dark: someone put the
     * phone face down or in a pocket to run it headless, and a game blaring
     * from the speaker is the audio version of the screen staying lit. Ringer,
     * alarm and notification streams are left alone on purpose - going quiet
     * on a call or an alarm because the mirror is open is the kind of surprise
     * this feature exists to avoid, same reasoning as never forcing the panel
     * on over a phone that dozed off for real.
     *
     * The wake lock built here is the other half of the same problem: forcing
     * the PANEL off does not tell Android's own idle timer anything. Input
     * arrives from the PC, not the touchscreen, so a stretch of watching
     * something rather than tapping still reads to Android as "nobody is
     * using this phone" - and past the user's screen-timeout, Android puts
     * itself to sleep for real, throttling the CPU under whatever is still
     * encoding video for the mirror. A PARTIAL_WAKE_LOCK keeps the CPU
     * running - the same mechanism a music player uses to keep playing with
     * the screen off - without turning the display or speaker back on,
     * which stay exactly as this feature already left them.
     */
    static void init(Context context) {
        try {
            audioManager = (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
        } catch (Throwable error) {
            Log.w(LOG_TAG, "audio manager unavailable " + error);
        }
        try {
            PowerManager powerManager =
                    (PowerManager) context.getSystemService(Context.POWER_SERVICE);
            wakeLock = powerManager.newWakeLock(
                    PowerManager.PARTIAL_WAKE_LOCK, "mirryl:panel-off");
            wakeLock.setReferenceCounted(false);
        } catch (Throwable error) {
            Log.w(LOG_TAG, "wake lock unavailable " + error);
        }
    }

    /**
     * Registers who hears about state changes, or null to stop.
     *
     * This exists because the mode can end WITHOUT the PC asking - the watcher
     * below lets go when Android moves the display. Without something pushing
     * that back, the window would keep showing "the screen is off" over a phone
     * that had woken up minutes ago, which is worse than showing nothing: the
     * warning it carries is about a state the phone is no longer in.
     */
    static void setListener(Listener next) {
        listener = next;
    }

    private static void announce(boolean off) {
        Listener current = listener;
        if (current == null) {
            return;
        }
        try {
            current.onPanelStateChanged(off);
        } catch (Throwable error) {
            // A dead socket is the normal way this fails, and it happens on the
            // teardown path where the panel still has to be given back.
            Log.i(LOG_TAG, "listener failed " + error);
        }
    }

    /**
     * Called for EVERY record that arrives from the PC, so anything the PC
     * sends counts as proof it is still there - not just the pings.
     */
    static void noteHostAlive() {
        lastHostContactMs = SystemClock.elapsedRealtime();
    }

    static boolean isEngaged() {
        return engaged;
    }

    /**
     * Flips the mode, and the reason this exists instead of the host tracking
     * its own idea of the state: this class LETS GO ON ITS OWN when Android
     * moves the display. A window that remembered "the screen is off" would
     * still be holding that belief after the phone dozed and woke, and the
     * user's next press would be spent turning off something already on. The
     * phone is the only side that knows the truth, so the phone decides.
     */
    static synchronized boolean toggle() {
        return setEngaged(!engaged);
    }

    /**
     * Turns the panel off, or gives it back. Returns false when the framework
     * refused, so the host can tell the difference between "done" and "this
     * phone will not do it" instead of leaving the user pressing a dead key.
     */
    static synchronized boolean setEngaged(boolean off) {
        if (off == engaged) {
            return true;
        }
        if (off) {
            if (!applyPowerMode(POWER_MODE_OFF)) {
                return false;
            }
            engaged = true;
            muteMedia();
            acquireWakeLock();
            // Start the clock from now. Whatever the PC last said, the mode
            // begins with it considered present, or the watchdog below would
            // fire on a session that had simply been quiet until this press.
            noteHostAlive();
            startWatcher();
            Log.i(LOG_TAG, "panel off");
            announce(true);
            return true;
        }
        engaged = false;
        return releaseToAndroidState("host asked");
    }

    /**
     * Gives the panel back because the session died.
     *
     * Called from the control socket's teardown, which is the one place that
     * finds out when the PC vanishes - a yanked cable, a dropped Wi-Fi, a PC
     * switched off at the wall. The PC cannot report its own death; the phone
     * notices its socket close. The same block already injects a pointer
     * cancel for the same reason.
     */
    static synchronized void restoreIfEngaged(String reason) {
        if (!engaged) {
            return;
        }
        engaged = false;
        releaseToAndroidState(reason);
    }

    /**
     * Puts the power mode back in step with Android's own idea of the display.
     * On an awake phone that turns the panel on; on one that dozed off while
     * the mode was held, it leaves the screen dark, which is what the person
     * holding the phone expects.
     */
    private static boolean releaseToAndroidState(String reason) {
        int mode = powerModeForState(displayState());
        boolean applied = applyPowerMode(mode);
        unmuteMedia();
        releaseWakeLock();
        Log.i(LOG_TAG, "panel restored mode=" + mode + " applied=" + applied
                + " reason=" + reason);
        announce(false);
        return applied;
    }

    /**
     * Mutes music/game audio the same way the hardware volume-down-to-zero
     * would, via ADJUST_MUTE rather than reading and reapplying a volume
     * level - Android already tracks the muted flag separately from the
     * level, so there is nothing here to remember or get out of sync if some
     * other app changes the volume while the panel is off.
     */
    private static void muteMedia() {
        AudioManager manager = audioManager;
        if (manager == null) {
            return;
        }
        try {
            manager.adjustStreamVolume(
                    AudioManager.STREAM_MUSIC, AudioManager.ADJUST_MUTE, 0);
        } catch (Throwable error) {
            Log.w(LOG_TAG, "mute failed " + error);
        }
    }

    private static void unmuteMedia() {
        AudioManager manager = audioManager;
        if (manager == null) {
            return;
        }
        try {
            manager.adjustStreamVolume(
                    AudioManager.STREAM_MUSIC, AudioManager.ADJUST_UNMUTE, 0);
        } catch (Throwable error) {
            Log.w(LOG_TAG, "unmute failed " + error);
        }
    }

    /**
     * Keeps the CPU from sleeping while the panel is held off, regardless of
     * how long it has been since the last real input event. Released the
     * instant the panel is given back (§releaseToAndroidState), including on
     * every safety-net path (host went quiet, session torn down) - never held
     * past the feature it exists for.
     */
    private static void acquireWakeLock() {
        PowerManager.WakeLock lock = wakeLock;
        if (lock == null || lock.isHeld()) {
            return;
        }
        try {
            lock.acquire();
        } catch (Throwable error) {
            Log.w(LOG_TAG, "wake lock acquire failed " + error);
        }
    }

    private static void releaseWakeLock() {
        PowerManager.WakeLock lock = wakeLock;
        if (lock == null || !lock.isHeld()) {
            return;
        }
        try {
            lock.release();
        } catch (Throwable error) {
            Log.w(LOG_TAG, "wake lock release failed " + error);
        }
    }

    private static int powerModeForState(int state) {
        switch (state) {
            case STATE_OFF:
                return POWER_MODE_OFF;
            case STATE_DOZE:
                return POWER_MODE_DOZE;
            case STATE_DOZE_SUSPEND:
                return POWER_MODE_DOZE_SUSPEND;
            case STATE_ON:
            default:
                // Includes the unknown/unreadable case on purpose: a screen
                // stuck ON is a nuisance, a screen stuck OFF looks like a
                // bricked phone.
                return POWER_MODE_NORMAL;
        }
    }

    /**
     * Watches for Android moving the display under us and lets go when it does.
     *
     * Polled rather than event-driven for the same reason the rotation watcher
     * is: this is a shell process with no Activity to receive callbacks and no
     * Looper thread it would otherwise need.
     */
    private static void startWatcher() {
        Thread watcher = new Thread(new Runnable() {
            @Override
            public void run() {
                int previous = displayState();
                while (engaged) {
                    try {
                        Thread.sleep(WATCH_POLL_MS);
                    } catch (InterruptedException interrupted) {
                        return;
                    }
                    // The PC has gone quiet: cable pulled, Wi-Fi gone, machine
                    // switched off at the wall, or a window that froze. None
                    // of those are worth telling apart - all of them mean
                    // nobody is looking at this phone's picture any more, and
                    // it must not be left holding a dark screen.
                    if (SystemClock.elapsedRealtime() - lastHostContactMs
                            > HOST_SILENCE_MS) {
                        Log.i(LOG_TAG, "host went quiet");
                        restoreIfEngaged("host went quiet");
                        return;
                    }
                    int state = displayState();
                    if (state == previous) {
                        continue;
                    }
                    Log.i(LOG_TAG, "display state moved " + previous + " -> " + state);
                    previous = state;
                    restoreIfEngaged("android moved the display");
                    return;
                }
            }
        }, "mirryl-panel-watch");
        watcher.setDaemon(true);
        watcher.start();
    }

    /**
     * What Android thinks the built-in display is doing, or 0 when it cannot be
     * read. Note this reports STATE_ON while the panel is forced off: Android
     * is not told about the mode this class sets, which is precisely why a
     * change in this value is a reliable sign that something ELSE moved.
     */
    private static int displayState() {
        try {
            Class<?> globalType =
                    Class.forName("android.hardware.display.DisplayManagerGlobal");
            Object global = globalType.getMethod("getInstance").invoke(null);
            Object info = globalType
                    .getMethod("getDisplayInfo", int.class)
                    .invoke(global, 0);
            if (info == null) {
                return 0;
            }
            return info.getClass().getField("state").getInt(info);
        } catch (Throwable ignored) {
            // Framework internals move between Android versions. Losing the
            // watcher costs the automatic release, not the feature.
            return 0;
        }
    }

    private static boolean applyPowerMode(int mode) {
        try {
            IBinder token = displayToken();
            if (token == null) {
                Log.w(LOG_TAG, "no display token");
                return false;
            }
            Class<?> surfaceControl = Class.forName("android.view.SurfaceControl");
            Method setter = surfaceControl.getMethod(
                    "setDisplayPowerMode", IBinder.class, int.class);
            setter.setAccessible(true);
            setter.invoke(null, token, mode);
            return true;
        } catch (Throwable error) {
            Log.w(LOG_TAG, "power mode " + mode + " failed " + error);
            return false;
        }
    }

    /**
     * The built-in panel's SurfaceFlinger token.
     *
     * Straight off SurfaceControl, which still carries both methods (checked
     * on a Samsung running Android 16). DisplayControl in services.jar is a
     * dead end here: loading libandroid_servers.so fails because it wants
     * libadb_pairing_server.so, which the device does not ship.
     */
    private static IBinder displayToken() throws Exception {
        Class<?> surfaceControl = Class.forName("android.view.SurfaceControl");
        long[] ids = (long[]) surfaceControl.getMethod("getPhysicalDisplayIds").invoke(null);
        if (ids == null || ids.length == 0) {
            return null;
        }
        Method token = surfaceControl.getMethod("getPhysicalDisplayToken", long.class);
        return (IBinder) token.invoke(null, ids[0]);
    }
}
