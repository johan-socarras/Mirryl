package dev.mirador.agent;

import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.AttributionSource;
import android.content.Context;
import android.content.ContextWrapper;
import android.os.Build;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import java.lang.reflect.Constructor;
import java.nio.charset.StandardCharsets;

/**
 * The phone's clipboard, both ways, while a mirror window is open.
 *
 * PC to phone: the text is put on the phone's clipboard and the agent presses
 * Ctrl+V there (see KIND_CLIPBOARD in ControlStreamMain), so it lands in one
 * piece - emoji included, which typing could not do - and stays on the phone
 * to be pasted again from its own menu.
 *
 * Phone to PC: every time the phone's clipboard changes while a PC is
 * connected, the text goes to the PC. That covers Ctrl+C from the PC and the
 * phone's own "Copy" button alike.
 *
 * Text only, and nothing is read with no PC connected: the listener checks for
 * a sink BEFORE reading, so an agent idling after the window closed (see
 * IDLE_EXIT_MS) never looks at the clipboard.
 *
 * Works from here because the agent runs as `shell`, which Android grants
 * READ_CLIPBOARD_IN_BACKGROUND (checked on a Samsung, Android 16) - the one
 * permission that lets something not in the foreground read the clipboard
 * since Android 10.
 */
final class ClipboardBridge {
    /** Where the phone's copies go: the status channel back to the PC. */
    interface Sink {
        void onPhoneClipboard(byte[] utf8);
    }

    /**
     * Largest copy carried either way, in UTF-8 bytes: 64 KB, about thirty
     * pages of text (a link is about 0.1 KB).
     * MUST MATCH MAX_CLIPBOARD_BYTES in the Rust host.
     */
    static final int MAX_BYTES = 64 * 1024;

    private static ClipboardManager manager;
    private static volatile Sink sink;
    private static String logTag = "MirrylControl";
    private static String sessionToken = "unknown";
    /**
     * Android reports ONE change more than once: on a Samsung every copy
     * fires the listener twice, and the PC's own paste would come
     * straight back. So a text already on the PC side - the one the PC
     * just put here, or the one just sent - is not sent again within this
     * window. Short on purpose: copying the same text again later still goes
     * through, in case the PC's clipboard changed in between.
     */
    private static final long REPEAT_WINDOW_MS = 2000;
    private static String recentText;
    private static long recentAtMs;

    private ClipboardBridge() {
    }

    /**
     * Builds a ClipboardManager whose change callbacks run on a thread of our
     * own. The one getSystemService would hand out posts them to the main
     * thread's looper, and this process's main thread never loops - it sits
     * in accept() - so the listener would simply never fire.
     *
     * Failure is logged and left at that: the rest of the agent (touch, keys,
     * typing) does not depend on the clipboard, and Ctrl+V falls back to
     * typing when there is no clipboard to set.
     */
    static void init(Context context, String tag, String token) {
        logTag = tag;
        sessionToken = token;
        try {
            HandlerThread thread = new HandlerThread("mirryl-clipboard");
            thread.setDaemon(true);
            thread.start();
            Handler handler = new Handler(thread.getLooper());
            Constructor<ClipboardManager> constructor =
                    ClipboardManager.class.getDeclaredConstructor(Context.class, Handler.class);
            constructor.setAccessible(true);
            Context shell = Build.VERSION.SDK_INT >= 31
                    ? new ShellIdentity31(context)
                    : new ShellIdentity(context);
            manager = constructor.newInstance(shell, handler);
            manager.addPrimaryClipChangedListener(
                    new ClipboardManager.OnPrimaryClipChangedListener() {
                        @Override
                        public void onPrimaryClipChanged() {
                            forwardToPc();
                        }
                    });
            Log.i(logTag, sessionToken + " clipboard_ready");
        } catch (Throwable error) {
            manager = null;
            Log.w(logTag, sessionToken + " clipboard_unavailable " + error);
        }
    }

    /**
     * The clipboard service checks that the package a caller names belongs to
     * the caller's uid. The agent's context names "android" (it is derived
     * from the system context) while running as shell, uid 2000, and the
     * service refuses it outright: "Package android does not belong to 2000".
     * ClipboardManager asks the context
     * for its package on every call, so this wrapper gives the right answer.
     */
    private static class ShellIdentity extends ContextWrapper {
        static final String SHELL_PACKAGE = "com.android.shell";

        ShellIdentity(Context base) {
            super(base);
        }

        @Override
        public String getPackageName() {
            return SHELL_PACKAGE;
        }

        @Override
        public String getOpPackageName() {
            return SHELL_PACKAGE;
        }
    }

    /** Android 12+ also carries the package inside the attribution source. */
    private static final class ShellIdentity31 extends ShellIdentity {
        ShellIdentity31(Context base) {
            super(base);
        }

        @Override
        public AttributionSource getAttributionSource() {
            return new AttributionSource.Builder(Process.myUid())
                    .setPackageName(SHELL_PACKAGE)
                    .build();
        }
    }

    /** null when no PC is connected - the listener then reads nothing. */
    static void setSink(Sink target) {
        sink = target;
    }

    /**
     * Puts the PC's text on the phone's clipboard. False if Android refused or
     * the clipboard never came up, so the caller can fall back to typing.
     */
    static boolean setFromPc(String text) {
        if (manager == null) {
            return false;
        }
        noteOnPc(text);
        try {
            manager.setPrimaryClip(ClipData.newPlainText("Mirryl", text));
            return true;
        } catch (Throwable error) {
            noteOnPc(null);
            Log.w(logTag, sessionToken + " clipboard_set_failed " + error);
            return false;
        }
    }

    private static void forwardToPc() {
        Sink target = sink;
        if (target == null || manager == null) {
            return;
        }
        String text;
        try {
            ClipData clip = manager.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) {
                return;
            }
            CharSequence chars = clip.getItemAt(0).getText();
            // An image or a file: not text, so not carried.
            if (chars == null) {
                return;
            }
            text = chars.toString();
        } catch (Throwable error) {
            Log.w(logTag, sessionToken + " clipboard_read_failed " + error);
            return;
        }
        if (text.isEmpty()) {
            return;
        }
        synchronized (ClipboardBridge.class) {
            if (text.equals(recentText)
                    && SystemClock.elapsedRealtime() - recentAtMs < REPEAT_WINDOW_MS) {
                return;
            }
        }
        byte[] utf8 = text.getBytes(StandardCharsets.UTF_8);
        // Sizes only, never the text: the session log keeps the agent's lines.
        if (utf8.length > MAX_BYTES) {
            Log.i(logTag, sessionToken + " clipboard_to_pc_too_big bytes=" + utf8.length);
            return;
        }
        noteOnPc(text);
        target.onPhoneClipboard(utf8);
    }

    private static synchronized void noteOnPc(String text) {
        recentText = text;
        recentAtMs = SystemClock.elapsedRealtime();
    }
}
