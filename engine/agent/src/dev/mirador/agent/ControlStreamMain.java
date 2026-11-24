package dev.mirador.agent;

import android.content.Context;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Looper;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;
import android.view.InputDevice;
import android.view.InputEvent;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.MotionEvent.PointerCoords;
import android.view.MotionEvent.PointerProperties;

import java.io.DataInputStream;
import java.io.EOFException;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.lang.reflect.Method;

public final class ControlStreamMain {
    private static final String LOG_TAG = "MirrylControl";
    private static final String SOCKET_PREFIX = "mirryl_control_";
    private static final int CONTROL_MAGIC = 0x4d594331; // MYC1
    private static final int CONTROL_VERSION = 1;
    /**
     * Records travelling the OTHER way, phone to PC, on the same socket.
     *
     * A different magic from the records coming in, so a confusion between the
     * two directions cannot be mistaken for a valid message on either side.
     */
    private static final int STATUS_MAGIC = 0x4d595331; // MYS1
    private static final int STATUS_VERSION = 1;
    private static final int STATUS_KIND_PANEL = 1;
    private static final int STATUS_RECORD_SIZE = 8;

    /**
     * How long this agent waits with nobody connected before terminating
     * itself.
     *
     * Why it exists: closing the mirror on the PC shuts the sockets and kills
     * the LOCAL adb children, but nothing there kills THIS process (see
     * viewer.rs, `kill_remote_agents`). Without a timer every session could
     * leave a listener behind, with `shell` identity, able to inject touches,
     * keys and text and to blank the panel, piling up until the phone was
     * rebooted. A capability that outlives the user closing the window is the
     * problem, whether or not anyone is in a position to reach it.
     *
     * The PC also kills us on a clean close (`stop_session`), which handles
     * the ordinary case at once. This timer is for the cases the PC cannot
     * handle at all: cable pulled, machine switched off at the wall, host
     * process crashed.
     *
     * Five minutes, not five seconds, on purpose. The host's own reconnect
     * window is 30 s, and when it does come back it kills this agent and starts
     * a fresh one anyway (`kill_remote_agents` before `start_control_agent`).
     * So nothing legitimate needs us alive while idle - the generous margin is
     * only so a slow reconnect can never be mistaken for an abandoned session.
     * It bounds the exposure from "forever" to "five minutes, one at a time".
     */
    private static final long IDLE_EXIT_MS = 5 * 60 * 1000L;
    private static final long IDLE_POLL_MS = 15 * 1000L;

    /**
     * How long an accepted client may stay silent before it is treated as
     * gone. The host sends a ping every second (PANEL_PING_INTERVAL), so
     * thirty missed ones is not a slow link, it is no host.
     *
     * Why the idle timer above is not enough: when the host process is killed
     * outright, the `adb` children it started on the PC can outlive it and keep
     * the forward's TCP socket open, so adbd never closes our end, `readInt`
     * below never returns, `clientConnected` stays true, and this agent would
     * sit listening with `shell` identity for hours. A read timeout does not
     * depend on anybody closing anything.
     */
    private static final int HOST_SILENCE_EXIT_MS = 30 * 1000;

    /** `elapsedRealtime` of the moment the last client left, or of start-up. */
    private static volatile long idleSinceMs = SystemClock.elapsedRealtime();

    /** True between `accept` returning and the client socket closing. */
    private static volatile boolean clientConnected = false;

    /**
     * Terminates the process once it has sat with no client for
     * {@link #IDLE_EXIT_MS}.
     *
     * A watchdog thread rather than a socket timeout because
     * `LocalServerSocket` has no accept timeout to set - there is no
     * `setSoTimeout` on it, only on the accepted client. Daemon, so it can
     * never be the reason the process stays up.
     *
     * The panel does not need restoring from here: `PanelPower` has its own
     * 3-second host-silence watchdog and has already given the screen back long
     * before this fires.
     */
    private static void startIdleWatchdog(final String sessionToken) {
        Thread watchdog = new Thread(new Runnable() {
            @Override
            public void run() {
                while (true) {
                    try {
                        Thread.sleep(IDLE_POLL_MS);
                    } catch (InterruptedException interrupted) {
                        return;
                    }
                    if (clientConnected) {
                        continue;
                    }
                    if (SystemClock.elapsedRealtime() - idleSinceMs > IDLE_EXIT_MS) {
                        Log.i(LOG_TAG, sessionToken + " idle_exit");
                        System.exit(0);
                    }
                }
            }
        }, "mirryl-control-idle");
        watchdog.setDaemon(true);
        watchdog.start();
    }
    private static final int KIND_POINTER = 1;
    private static final int KIND_KEY = 2;
    private static final int KIND_TEXT = 3;
    /** Panel power: action 0 gives the screen back, 1 turns it off, 2 flips. */
    private static final int KIND_PANEL = 4;
    private static final int PANEL_TOGGLE = 2;
    /**
     * "Still here." Carries nothing; arriving at all is the message.
     *
     * The PC sends these while it is holding the panel off, and the watchdog in
     * PanelPower gives the screen back when they stop. See HOST_SILENCE_MS
     * there for why silence, rather than a closed socket, is what we trust.
     */
    private static final int KIND_PING = 5;
    /**
     * Two fingers at once - the one gesture a single MotionEvent pointer can
     * never fake, because pinch-to-zoom reads the DISTANCE between two
     * touches, not where either one is by itself. action: 0 begin (both
     * fingers touch down), 1 move (both fingers reposition), 2 end (both
     * fingers lift), 3 cancel (link died mid-pinch).
     *
     * argument1/argument2 are finger 1's (x, y), same normalized scale as
     * KIND_POINTER. Finger 2 has nowhere left in the fixed record - a third
     * coordinate pair does not fit alongside a third int argument - so it
     * rides inside argument3, packed as two 16-bit halves: x2 in the high
     * bits, y2 in the low bits. POINTER_SCALE (10 000) fits in 14 of those
     * 16 bits with room to spare. MUST MATCH how the Rust host packs it.
     */
    private static final int KIND_PINCH = 6;
    private static final int PINCH_BEGIN = 0;
    private static final int PINCH_MOVE = 1;
    private static final int PINCH_END = 2;
    private static final int PINCH_CANCEL = 3;
    /**
     * A real Android scroll event (ACTION_SCROLL / AXIS_VSCROLL) - what a
     * mouse's wheel already looks like to Android - rather than faking a
     * swipe for every tick of the wheel. More reliable for lists and pages,
     * and the same right-click=Back mapping this app already follows.
     *
     * argument1/argument2 are where the pointer sits (normalized, same as
     * KIND_POINTER); argument3 is the scroll amount, AXIS_VSCROLL's own
     * units (roughly 1.0 per physical notch) fixed-point at x1000 so a
     * fraction of a notch survives the trip as a signed int. MUST MATCH the
     * Rust host's encoding.
     */
    private static final int KIND_SCROLL = 7;
    private static final int SCROLL_FIXED_POINT = 1000;
    /**
     * The PC's clipboard text, to be put on the phone's clipboard - see
     * ClipboardBridge. argument1 is the UTF-8 byte count and the text follows
     * the fixed header, same layout as KIND_TEXT. action CLIPBOARD_PASTE also
     * presses Ctrl+V once it is set: that is what Ctrl+V in the mirror window
     * sends. MUST MATCH CONTROL_KIND_CLIPBOARD in the Rust host.
     */
    private static final int KIND_CLIPBOARD = 8;
    private static final int CLIPBOARD_PASTE = 1;
    private static final int STATUS_KIND_CLIPBOARD = 2;
    private static final int INJECT_ASYNC = 0;
    /**
     * Pointer coordinates arrive normalized to this range instead of in pixels.
     * Must match POINTER_SCALE in the Rust host.
     */
    private static final int POINTER_SCALE = 10_000;
    /** Bounds a single text record so a malformed length cannot allocate wildly. */
    private static final int MAX_TEXT_BYTES = 4096;

    private ControlStreamMain() {
    }

    public static void main(String[] arguments) {
        // First thing, before any work: the PC times VM start-up and the
        // set-up below separately. See HostSignal.
        HostSignal.alive();
        String sessionToken = arguments.length > 0 ? arguments[0] : "unknown";
        HostSignal.logUncaught(LOG_TAG, sessionToken);
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_DISPLAY);
            Context context = shellContext();
            PanelPower.init(context);
            ClipboardBridge.init(context, LOG_TAG, sessionToken);
            Object inputManager = context.getSystemService(Context.INPUT_SERVICE);
            if (inputManager == null) {
                throw new IllegalStateException("InputManager is unavailable");
            }
            Method inject = inputManager.getClass().getMethod(
                    "injectInputEvent",
                    InputEvent.class,
                    int.class);
            inject.setAccessible(true);

            String socketName = SOCKET_PREFIX + sessionToken;
            try (LocalServerSocket server = new LocalServerSocket(socketName)) {
                FileOutputStream ready = new FileOutputStream(FileDescriptor.out);
                ready.write(0x7f);
                ready.flush();
                Log.i(LOG_TAG, sessionToken + " ready socket=" + socketName);
                // Armed before the first accept, so an agent that is started and
                // then never reached - the host failed between launching us and
                // connecting - also goes away instead of listening for ever.
                startIdleWatchdog(sessionToken);

                while (true) {
                    try (LocalSocket client = HostSignal.acceptFromHost(server, LOG_TAG, sessionToken)) {
                        clientConnected = true;
                        // SO_RCVTIMEO on the accepted socket: a read that
                        // waits longer than this throws, the try-with-resources
                        // closes the client, and the idle clock takes over.
                        client.setSoTimeout(HOST_SILENCE_EXIT_MS);
                        Log.i(LOG_TAG, sessionToken + " client_connected");
                        // The PC hears about the panel down the same socket it
                        // sends input on. It has to hear: this agent gives the
                        // panel back on its own when Android moves the display,
                        // and a window that never learned would keep warning
                        // about a screen that is on again.
                        StatusWriter status =
                                new StatusWriter(client.getOutputStream(), sessionToken);
                        PanelPower.setListener(status);
                        ClipboardBridge.setSink(status);
                        try {
                            serveClient(
                                    new DataInputStream(client.getInputStream()),
                                    inputManager,
                                    inject,
                                    sessionToken);
                        } catch (EOFException ignored) {
                            Log.i(LOG_TAG, sessionToken + " client_disconnected");
                        } catch (java.io.IOException lost) {
                            // The read timeout above lands here ("Try again"),
                            // as does a socket that broke under us.
                            Log.i(LOG_TAG, sessionToken + " client_lost " + lost.getMessage());
                        } finally {
                            PanelPower.setListener(null);
                            ClipboardBridge.setSink(null);
                        }
                    } finally {
                        // Whatever ended the client - clean disconnect, error,
                        // or the socket closing under us - the idle clock starts
                        // here.
                        clientConnected = false;
                        idleSinceMs = SystemClock.elapsedRealtime();
                    }
                }
            }
        } catch (Throwable error) {
            Log.e(LOG_TAG, sessionToken + " fatal " + error, error);
            error.printStackTrace(System.err);
        }
    }

    private static void serveClient(
            DataInputStream input,
            Object inputManager,
            Method inject,
            String sessionToken)
            throws Exception {
        long downTime = 0;
        int lastX = 0;
        int lastY = 0;
        // Pinch has its own down-state, kept separate from the single-finger
        // one above: the two gestures are mutually exclusive (see the guards
        // in the dispatch below), but they are not the SAME state - a pinch
        // tracks two fingers' last known position for its own disconnect
        // cleanup, which a single int pair cannot hold.
        boolean pinchActive = false;
        long pinchDownTime = 0;
        int lastPinchX1 = 0;
        int lastPinchY1 = 0;
        int lastPinchX2 = 0;
        int lastPinchY2 = 0;
        long reportStartedNs = System.nanoTime();
        long intervalRecords = 0;
        long totalRecords = 0;
        long maxInjectUs = 0;
        try {
            while (true) {
                int magic = input.readInt();
                int version = input.readUnsignedByte();
                int kind = input.readUnsignedByte();
                int action = input.readUnsignedByte();
                input.readUnsignedByte();
                int argument1 = input.readInt();
                int argument2 = input.readInt();
                int argument3 = input.readInt();
                input.readInt(); // sequence

                if (magic != CONTROL_MAGIC || version != CONTROL_VERSION) {
                    throw new IllegalStateException("invalid control record");
                }
                // Anything arriving at all proves the PC is still there, not
                // just the pings, so a busy session never trips the watchdog.
                PanelPower.noteHostAlive();
                long now = SystemClock.uptimeMillis();
                long beforeInjectNs = System.nanoTime();
                if (kind == KIND_POINTER) {
                    // A pinch already owns the touchscreen's one finger slot
                    // this protocol models as pointer id 0 - see the mutual
                    // exclusion note on KIND_PINCH. The host should never send
                    // both at once, but a message that slipped past that must
                    // not be allowed to inject a THIRD finger into a gesture
                    // Android is already mid-way through.
                    if (pinchActive) {
                        continue;
                    }
                    // Coordinates arrive NORMALIZED (0..POINTER_SCALE), not in
                    // pixels: the host only knows the phone's PHYSICAL size -
                    // always portrait shaped, fixed for the whole session - so
                    // on a turned phone its taps would land in the wrong place.
                    // Scaling here is the only place that knows
                    // where the display is pointing right now.
                    int[] display = VideoStreamMain.lastDisplaySize();
                    if (display != null) {
                        argument1 = Math.round(
                                argument1 / (float) POINTER_SCALE * (display[0] - 1));
                        argument2 = Math.round(
                                argument2 / (float) POINTER_SCALE * (display[1] - 1));
                    }
                    lastX = argument1;
                    lastY = argument2;
                    if (action == MotionEvent.ACTION_DOWN) {
                        downTime = now;
                    } else if (downTime == 0) {
                        continue;
                    }
                    injectPointer(
                            inputManager,
                            inject,
                            downTime,
                            now,
                            action,
                            argument1,
                            argument2);
                    if (action == MotionEvent.ACTION_UP
                            || action == MotionEvent.ACTION_CANCEL) {
                        downTime = 0;
                    }
                } else if (kind == KIND_PINCH) {
                    // The mirror image of the guard above: a pinch cannot
                    // begin while the single-finger slot is already down, and
                    // any move/end/cancel that arrives without a begin having
                    // been accepted (dropped by the guard, or simply never
                    // sent) has nothing to act on.
                    if (action == PINCH_BEGIN) {
                        if (downTime != 0 || pinchActive) {
                            continue;
                        }
                        int[] display = VideoStreamMain.lastDisplaySize();
                        int x1 = argument1;
                        int y1 = argument2;
                        int x2 = (argument3 >>> 16) & 0xFFFF;
                        int y2 = argument3 & 0xFFFF;
                        if (display != null) {
                            x1 = Math.round(x1 / (float) POINTER_SCALE * (display[0] - 1));
                            y1 = Math.round(y1 / (float) POINTER_SCALE * (display[1] - 1));
                            x2 = Math.round(x2 / (float) POINTER_SCALE * (display[0] - 1));
                            y2 = Math.round(y2 / (float) POINTER_SCALE * (display[1] - 1));
                        }
                        pinchActive = true;
                        pinchDownTime = now;
                        lastPinchX1 = x1;
                        lastPinchY1 = y1;
                        lastPinchX2 = x2;
                        lastPinchY2 = y2;
                        // Android expects to SEE the first finger arrive alone
                        // before the second joins it - a single MotionEvent
                        // claiming two fingers appeared at once is not a
                        // sequence any real touchscreen would ever produce,
                        // and views that key off ACTION_DOWN specifically
                        // (as opposed to ACTION_POINTER_DOWN) would never see
                        // the gesture start at all.
                        injectPinch(
                                inputManager, inject, pinchDownTime, now,
                                MotionEvent.ACTION_DOWN, 1, x1, y1, x2, y2);
                        injectPinch(
                                inputManager, inject, pinchDownTime, now,
                                MotionEvent.ACTION_POINTER_DOWN
                                        | (1 << MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                                2, x1, y1, x2, y2);
                    } else if (pinchActive) {
                        int[] display = VideoStreamMain.lastDisplaySize();
                        int x1 = argument1;
                        int y1 = argument2;
                        int x2 = (argument3 >>> 16) & 0xFFFF;
                        int y2 = argument3 & 0xFFFF;
                        if (display != null) {
                            x1 = Math.round(x1 / (float) POINTER_SCALE * (display[0] - 1));
                            y1 = Math.round(y1 / (float) POINTER_SCALE * (display[1] - 1));
                            x2 = Math.round(x2 / (float) POINTER_SCALE * (display[0] - 1));
                            y2 = Math.round(y2 / (float) POINTER_SCALE * (display[1] - 1));
                        }
                        lastPinchX1 = x1;
                        lastPinchY1 = y1;
                        lastPinchX2 = x2;
                        lastPinchY2 = y2;
                        if (action == PINCH_MOVE) {
                            injectPinch(
                                    inputManager, inject, pinchDownTime, now,
                                    MotionEvent.ACTION_MOVE, 2, x1, y1, x2, y2);
                        } else if (action == PINCH_END) {
                            injectPinch(
                                    inputManager, inject, pinchDownTime, now,
                                    MotionEvent.ACTION_POINTER_UP
                                            | (1 << MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                                    2, x1, y1, x2, y2);
                            injectPinch(
                                    inputManager, inject, pinchDownTime, now,
                                    MotionEvent.ACTION_UP, 1, x1, y1, 0, 0);
                            pinchActive = false;
                        } else if (action == PINCH_CANCEL) {
                            injectPinch(
                                    inputManager, inject, pinchDownTime, now,
                                    MotionEvent.ACTION_CANCEL, 2, x1, y1, x2, y2);
                            pinchActive = false;
                        }
                    }
                } else if (kind == KIND_SCROLL) {
                    int[] display = VideoStreamMain.lastDisplaySize();
                    int x = argument1;
                    int y = argument2;
                    if (display != null) {
                        x = Math.round(x / (float) POINTER_SCALE * (display[0] - 1));
                        y = Math.round(y / (float) POINTER_SCALE * (display[1] - 1));
                    }
                    injectScroll(
                            inputManager, inject, now, x, y,
                            argument3 / (float) SCROLL_FIXED_POINT);
                } else if (kind == KIND_KEY) {
                    // argument2 is the meta state: 0 for a plain key, Ctrl for
                    // the Ctrl+A/C/X/Z/V the mirror window passes through.
                    injectKey(inputManager, inject, now, argument1, argument2);
                } else if (kind == KIND_CLIPBOARD) {
                    int length = argument1;
                    if (length < 0 || length > ClipboardBridge.MAX_BYTES) {
                        throw new IllegalStateException("clipboard payload out of range");
                    }
                    byte[] payload = new byte[length];
                    input.readFully(payload);
                    String text = new String(payload, "UTF-8");
                    boolean set = ClipboardBridge.setFromPc(text);
                    Log.i(LOG_TAG, sessionToken + " clipboard_from_pc bytes=" + length
                            + " set=" + set);
                    if (action == CLIPBOARD_PASTE) {
                        if (set) {
                            injectKey(inputManager, inject, now, KeyEvent.KEYCODE_V,
                                    KeyEvent.META_CTRL_ON | KeyEvent.META_CTRL_LEFT_ON);
                        } else if (length <= MAX_TEXT_BYTES) {
                            // What Ctrl+V did before there was a clipboard to
                            // set: type it. Better than pasting nothing.
                            injectText(inputManager, inject, text, sessionToken);
                        }
                    }
                } else if (kind == KIND_TEXT) {
                    // Variable-length record: argument1 is the UTF-8 byte count
                    // and the payload follows the fixed header.
                    int length = argument1;
                    if (length < 0 || length > MAX_TEXT_BYTES) {
                        throw new IllegalStateException("text payload out of range");
                    }
                    byte[] payload = new byte[length];
                    input.readFully(payload);
                    injectText(inputManager, inject, new String(payload, "UTF-8"), sessionToken);
                } else if (kind == KIND_PING) {
                    // Nothing to do: noteHostAlive above was the whole point.
                    continue;
                } else if (kind == KIND_PANEL) {
                    boolean done = action == PANEL_TOGGLE
                            ? PanelPower.toggle()
                            : PanelPower.setEngaged(action != 0);
                    Log.i(
                            LOG_TAG,
                            sessionToken
                                    + " panel action="
                                    + action
                                    + " off="
                                    + PanelPower.isEngaged()
                                    + " done="
                                    + done);
                } else {
                    throw new IllegalStateException("unknown control kind");
                }
                maxInjectUs = Math.max(
                        maxInjectUs,
                        (System.nanoTime() - beforeInjectNs) / 1_000);
                intervalRecords++;
                totalRecords++;
                long nowNs = System.nanoTime();
                if (nowNs - reportStartedNs >= 1_000_000_000L) {
                    Log.i(
                            LOG_TAG,
                            sessionToken
                                    + " tick records="
                                    + intervalRecords
                                    + " total_records="
                                    + totalRecords
                                    + " max_inject_us="
                                    + maxInjectUs);
                    reportStartedNs = nowNs;
                    intervalRecords = 0;
                    maxInjectUs = 0;
                }
            }
        } finally {
            // The panel is given back HERE, on the phone, and not by the PC.
            // A PC that was switched off at the wall cannot ask for anything;
            // this socket dying is what a yanked cable, a dropped Wi-Fi and a
            // dead PC all look like from the phone. Same reasoning as the
            // pointer cancel just below, which has been doing this for longer.
            PanelPower.restoreIfEngaged("control socket closed");
            if (downTime != 0) {
                long now = SystemClock.uptimeMillis();
                try {
                    injectPointer(
                            inputManager,
                            inject,
                            downTime,
                            now,
                            MotionEvent.ACTION_CANCEL,
                            lastX,
                            lastY);
                    Log.i(LOG_TAG, sessionToken + " injected_cancel_on_disconnect");
                } catch (Throwable error) {
                    Log.w(LOG_TAG, sessionToken + " cancel_failed " + error);
                }
            }
            // Same reasoning, for two fingers instead of one: a link that
            // died mid-pinch must not leave Google Maps (or whatever app is
            // open) believing a finger is still down and never coming up.
            if (pinchActive) {
                long now = SystemClock.uptimeMillis();
                try {
                    injectPinch(
                            inputManager,
                            inject,
                            pinchDownTime,
                            now,
                            MotionEvent.ACTION_CANCEL,
                            2,
                            lastPinchX1,
                            lastPinchY1,
                            lastPinchX2,
                            lastPinchY2);
                    Log.i(LOG_TAG, sessionToken + " injected_pinch_cancel_on_disconnect");
                } catch (Throwable error) {
                    Log.w(LOG_TAG, sessionToken + " pinch_cancel_failed " + error);
                }
            }
        }
    }

    /**
     * Types real text on the device.
     *
     * Key codes alone cannot do this: there is no Android key code for "ñ" or
     * "@", and the letter codes need the right meta state for case. The
     * platform already solves it - KeyCharacterMap turns a string into the
     * exact KeyEvent sequence a physical keyboard would produce, meta state
     * included - so the host sends characters and lets Android decide the
     * events.
     *
     * getEvents() returns null if ANY character in the batch is unmappable, so
     * a failed batch is retried one character at a time: an emoji in the
     * middle of a sentence should cost that emoji, not the whole sentence.
     */
    private static void injectText(
            Object inputManager,
            Method inject,
            String text,
            String sessionToken)
            throws Exception {
        if (text.isEmpty()) {
            return;
        }
        KeyCharacterMap map = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD);
        if (injectChars(inputManager, inject, map, text.toCharArray())) {
            return;
        }
        int skipped = 0;
        for (int i = 0; i < text.length(); i++) {
            if (!injectChars(inputManager, inject, map, new char[] {text.charAt(i)})) {
                skipped++;
            }
        }
        if (skipped > 0) {
            Log.i(LOG_TAG, sessionToken + " text_unmappable_chars=" + skipped);
        }
    }

    /**
     * One key press, down and up. With Ctrl in `metaState` this is what makes
     * Ctrl+A/C/X/Z/V act in the phone's text fields: Android routes a key
     * whose meta state says Ctrl to the focused view as a shortcut, the same
     * as from a physical keyboard, without a separate Ctrl key event.
     */
    private static void injectKey(
            Object inputManager,
            Method inject,
            long now,
            int keyCode,
            int metaState)
            throws Exception {
        KeyEvent down = new KeyEvent(now, now, KeyEvent.ACTION_DOWN, keyCode, 0, metaState);
        KeyEvent up = new KeyEvent(now, now, KeyEvent.ACTION_UP, keyCode, 0, metaState);
        down.setSource(InputDevice.SOURCE_KEYBOARD);
        up.setSource(InputDevice.SOURCE_KEYBOARD);
        inject.invoke(inputManager, down, INJECT_ASYNC);
        inject.invoke(inputManager, up, INJECT_ASYNC);
    }

    private static boolean injectChars(
            Object inputManager,
            Method inject,
            KeyCharacterMap map,
            char[] chars)
            throws Exception {
        KeyEvent[] events = map.getEvents(chars);
        if (events == null) {
            return false;
        }
        for (KeyEvent event : events) {
            KeyEvent routed = KeyEvent.changeFlags(
                    event, event.getFlags() | KeyEvent.FLAG_SOFT_KEYBOARD);
            routed.setSource(InputDevice.SOURCE_KEYBOARD);
            inject.invoke(inputManager, routed, INJECT_ASYNC);
        }
        return true;
    }

    private static void injectPointer(
            Object inputManager,
            Method inject,
            long downTime,
            long eventTime,
            int action,
            int x,
            int y)
            throws Exception {
        float pressure =
                action == MotionEvent.ACTION_UP
                        || action == MotionEvent.ACTION_CANCEL
                        ? 0.0f
                        : 1.0f;
        MotionEvent event = MotionEvent.obtain(
                downTime,
                eventTime,
                action,
                x,
                y,
                pressure,
                1.0f,
                0,
                1.0f,
                1.0f,
                0,
                0);
        event.setSource(InputDevice.SOURCE_TOUCHSCREEN);
        inject.invoke(inputManager, event, INJECT_ASYNC);
        event.recycle();
    }

    /**
     * One step of a two-finger gesture. `pointerCount` is 1 for the moment
     * before the second finger has joined (the opening ACTION_DOWN) or after
     * it has already left (the closing ACTION_UP) - finger 2's coordinates
     * are ignored in that case, not just zero-filled and sent anyway, because
     * a MotionEvent with a phantom second pointer is exactly the malformed
     * shape a real touchscreen could never produce.
     */
    private static void injectPinch(
            Object inputManager,
            Method inject,
            long downTime,
            long eventTime,
            int action,
            int pointerCount,
            int x1,
            int y1,
            int x2,
            int y2)
            throws Exception {
        PointerProperties properties0 = new PointerProperties();
        properties0.id = 0;
        properties0.toolType = MotionEvent.TOOL_TYPE_FINGER;
        PointerCoords coordinates0 = new PointerCoords();
        coordinates0.x = x1;
        coordinates0.y = y1;
        coordinates0.pressure = 1.0f;
        coordinates0.size = 1.0f;

        PointerProperties[] properties;
        PointerCoords[] coordinates;
        if (pointerCount == 1) {
            properties = new PointerProperties[] {properties0};
            coordinates = new PointerCoords[] {coordinates0};
        } else {
            PointerProperties properties1 = new PointerProperties();
            properties1.id = 1;
            properties1.toolType = MotionEvent.TOOL_TYPE_FINGER;
            PointerCoords coordinates1 = new PointerCoords();
            coordinates1.x = x2;
            coordinates1.y = y2;
            coordinates1.pressure = 1.0f;
            coordinates1.size = 1.0f;
            properties = new PointerProperties[] {properties0, properties1};
            coordinates = new PointerCoords[] {coordinates0, coordinates1};
        }
        MotionEvent event = MotionEvent.obtain(
                downTime,
                eventTime,
                action,
                pointerCount,
                properties,
                coordinates,
                0,
                0,
                1.0f,
                1.0f,
                0,
                0,
                InputDevice.SOURCE_TOUCHSCREEN,
                0);
        inject.invoke(inputManager, event, INJECT_ASYNC);
        event.recycle();
    }

    /**
     * A real mouse-wheel scroll, not a faked drag.
     *
     * The source matters here in a way it does not for the other injectors:
     * every other event in this file claims SOURCE_TOUCHSCREEN because that
     * is genuinely what a finger is. A scroll wheel is not a touchscreen
     * gesture on any real device, and a view's onGenericMotionEvent only
     * treats AXIS_VSCROLL as a scroll when the event says it came from
     * SOURCE_CLASS_POINTER - which SOURCE_MOUSE already carries. Sending it
     * as a touch event would not crash anything; it would simply be ignored,
     * the same way a real phone ignores a mouse acting like a finger.
     */
    private static void injectScroll(
            Object inputManager,
            Method inject,
            long eventTime,
            int x,
            int y,
            float axisValue)
            throws Exception {
        PointerProperties properties = new PointerProperties();
        properties.id = 0;
        properties.toolType = MotionEvent.TOOL_TYPE_MOUSE;
        PointerCoords coordinates = new PointerCoords();
        coordinates.x = x;
        coordinates.y = y;
        coordinates.setAxisValue(MotionEvent.AXIS_VSCROLL, axisValue);
        MotionEvent event = MotionEvent.obtain(
                eventTime,
                eventTime,
                MotionEvent.ACTION_SCROLL,
                1,
                new PointerProperties[] {properties},
                new PointerCoords[] {coordinates},
                0,
                0,
                1.0f,
                1.0f,
                0,
                0,
                InputDevice.SOURCE_MOUSE,
                0);
        inject.invoke(inputManager, event, INJECT_ASYNC);
        event.recycle();
    }

    /**
     * Sends the panel's state back to the PC over the control socket.
     *
     * Fixed-size records with their own magic, for the same reason the incoming
     * ones have it: the reader on the other side needs to be able to tell a
     * message it understands from noise, without a length field to trust.
     *
     * Writes are synchronized because they arrive from two threads - the one
     * serving the PC's commands, and the panel watcher that lets go on its own
     * - and half of one record inside another would desynchronise the reader
     * for the rest of the session.
     */
    private static final class StatusWriter
            implements PanelPower.Listener, ClipboardBridge.Sink {
        private final OutputStream output;
        private final String sessionToken;

        StatusWriter(OutputStream output, String sessionToken) {
            this.output = output;
            this.sessionToken = sessionToken;
        }

        @Override
        public synchronized void onPanelStateChanged(boolean off) {
            byte[] record = new byte[STATUS_RECORD_SIZE];
            record[0] = (byte) (STATUS_MAGIC >>> 24);
            record[1] = (byte) (STATUS_MAGIC >>> 16);
            record[2] = (byte) (STATUS_MAGIC >>> 8);
            record[3] = (byte) STATUS_MAGIC;
            record[4] = (byte) STATUS_VERSION;
            record[5] = (byte) STATUS_KIND_PANEL;
            record[6] = (byte) (off ? 1 : 0);
            try {
                output.write(record);
                output.flush();
                Log.i(LOG_TAG, sessionToken + " status panel off=" + off);
            } catch (IOException error) {
                // Expected on the way out: the panel is given back BECAUSE this
                // socket died, so the message about it has nowhere to go. The
                // giving back already happened; only the telling failed.
                Log.i(LOG_TAG, sessionToken + " status not sent " + error);
            }
        }

        /**
         * The phone's copied text, for the PC's clipboard. The one record that
         * is not fixed-size: the usual 8-byte header with kind 2, then the
         * UTF-8 byte count as an int, then the text. The host reads exactly
         * that many bytes, so the records after it stay aligned.
         */
        @Override
        public synchronized void onPhoneClipboard(byte[] utf8) {
            byte[] header = new byte[STATUS_RECORD_SIZE + 4];
            header[0] = (byte) (STATUS_MAGIC >>> 24);
            header[1] = (byte) (STATUS_MAGIC >>> 16);
            header[2] = (byte) (STATUS_MAGIC >>> 8);
            header[3] = (byte) STATUS_MAGIC;
            header[4] = (byte) STATUS_VERSION;
            header[5] = (byte) STATUS_KIND_CLIPBOARD;
            header[8] = (byte) (utf8.length >>> 24);
            header[9] = (byte) (utf8.length >>> 16);
            header[10] = (byte) (utf8.length >>> 8);
            header[11] = (byte) utf8.length;
            try {
                output.write(header);
                output.write(utf8);
                output.flush();
                Log.i(LOG_TAG, sessionToken + " status clipboard bytes=" + utf8.length);
            } catch (IOException error) {
                Log.i(LOG_TAG, sessionToken + " clipboard not sent " + error);
            }
        }
    }

    private static Context shellContext() throws Exception {
        if (Looper.myLooper() == null) {
            Looper.prepare();
        }
        Class<?> activityThread = Class.forName("android.app.ActivityThread");
        Object thread = activityThread.getDeclaredMethod("systemMain").invoke(null);
        Context system =
                (Context) activityThread.getDeclaredMethod("getSystemContext").invoke(thread);
        return system.createPackageContext(
                "com.android.shell",
                Context.CONTEXT_IGNORE_SECURITY);
    }
}
