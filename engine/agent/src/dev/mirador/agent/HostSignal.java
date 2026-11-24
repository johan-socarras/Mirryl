package dev.mirador.agent;

import android.net.Credentials;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.util.Log;

import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.IOException;

/**
 * The few bytes an agent says to the PC on stdout before its socket exists.
 *
 * Every agent is started with `adb exec-out … app_process`, and until it opens
 * its localabstract socket, stdout is the only channel it has. The PC reads it
 * one byte at a time and acts on the first byte that is not {@link #ALIVE}:
 *
 * <ul>
 *   <li>{@link #ALIVE} - written first thing in `main`, before any work. It
 *       tells the PC that the VM has started and the agent's own code is
 *       running, so the PC can stop counting VM start-up against the agent
 *       and give the initialisation that follows its own, longer budget. A
 *       slow phone over Wi-Fi can take 4 s just to reach `main`, which
 *       would otherwise read as "the agent did not answer".</li>
 *   <li>{@link #READY} - the socket is listening; the PC creates the forward
 *       and connects.</li>
 *   <li>{@link #UNSUPPORTED} - this phone gives the agent no way to do its
 *       job. One UTF-8 line saying why follows, and the PC shows that line to
 *       the person instead of retrying, rather than relaunching it for
 *       ever behind a black window.</li>
 * </ul>
 *
 * The values are control characters on purpose, so they can never be mistaken
 * for text that adb itself might print.
 */
final class HostSignal {
    static final int ALIVE = 0x7d;
    static final int READY = 0x7f;
    static final int UNSUPPORTED = 0x7e;

    /** `shell`, the identity adbd gives every connection it forwards. */
    private static final int SHELL_UID = 2000;
    /** `adb root` on a userdebug build forwards as root instead. */
    private static final int ROOT_UID = 0;

    /**
     * Accepts connections until one comes from adbd - the only path the PC
     * ever uses - and closes every other peer without serving a byte.
     *
     * The three agent sockets live in the abstract namespace, where no file
     * permission applies: the name would be the whole access control. On the
     * phones measured SELinux already keeps every app away from them; this is
     * a second wall, so that a phone whose policy differs does not hand its
     * screen, audio and touch to the first app that learns the name. adbd
     * connects as `shell` (or root after `adb root`); a normal app never has
     * either uid, and the kernel, not the peer, reports it.
     */
    static LocalSocket acceptFromHost(LocalServerSocket server, String tag, String token)
            throws IOException {
        while (true) {
            LocalSocket client = server.accept();
            int uid = -1;
            try {
                Credentials peer = client.getPeerCredentials();
                if (peer != null) {
                    uid = peer.getUid();
                }
            } catch (IOException ignored) {
                // Unknown peer is treated like a wrong one.
            }
            if (uid == SHELL_UID || uid == ROOT_UID) {
                return client;
            }
            Log.w(tag, token + " rejected_peer uid=" + uid);
            try {
                client.close();
            } catch (IOException ignored) {
                // Nothing else to do with it.
            }
        }
    }

    private HostSignal() {
    }

    /** The VM is up and the agent is running its own code. */
    static void alive() {
        write(ALIVE, null);
    }

    /** The agent's socket is listening. */
    static void ready() {
        write(READY, null);
    }

    /** This phone cannot do the job, and this is why. Shown to the person as-is. */
    static void unsupported(String reason) {
        write(UNSUPPORTED, reason == null || reason.isEmpty() ? "not supported on this phone" : reason);
    }

    private static void write(int marker, String line) {
        try {
            FileOutputStream out = new FileOutputStream(FileDescriptor.out);
            out.write(marker);
            if (line != null) {
                out.write(line.replace('\r', ' ').replace('\n', ' ').getBytes("UTF-8"));
                out.write('\n');
            }
            out.flush();
        } catch (IOException ignored) {
            // stdout gone means the PC gave up on us; nothing left to tell it.
        }
    }

    /**
     * Makes sure a crash on ANY thread leaves a line in logcat with the
     * session token, which is the only thing the PC's log filter keeps. A
     * worker thread that dies silently would look like a stream that stopped
     * for no reason.
     */
    static void logUncaught(final String tag, final String sessionToken) {
        Thread.setDefaultUncaughtExceptionHandler(new Thread.UncaughtExceptionHandler() {
            @Override
            public void uncaughtException(Thread thread, Throwable error) {
                Log.e(tag, sessionToken + " fatal on " + thread.getName() + ": " + error, error);
            }
        });
    }
}
