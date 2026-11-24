package dev.mirador.agent;

import android.app.Application;
import android.app.Instrumentation;
import android.content.AttributionSource;
import android.content.Context;
import android.content.ContextWrapper;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioRecord;
import android.media.MediaRecorder;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Build;
import android.os.Looper;
import android.os.Process;
import android.util.Log;

import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.OutputStream;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;

public final class AudioStreamMain {
    private static final String LOG_TAG = "MirrylAudio";
    private static final String SOCKET_PREFIX = "mirryl_audio_";
    private static final int SAMPLE_RATE = 48_000;
    /** Stdout byte that tells the host the socket is listening. */
    private static final int READY_BYTE = 0x7f;
    /**
     * Stdout byte that tells the host this phone gives a shell process no way
     * to capture audio, followed by one UTF-8 line saying why. The host stops
     * asking for audio for the rest of the session instead of relaunching this
     * agent every few seconds; the picture is unaffected.
     */
    private static final int UNSUPPORTED_BYTE = 0x7e;
    /** First release whose shell holds MODIFY_AUDIO_ROUTING: the duplicating mix. */
    private static final int FIRST_API_WITH_DUPLICATE = 33;
    /** First release whose shell holds RECORD_AUDIO and CAPTURE_AUDIO_OUTPUT. */
    private static final int FIRST_API_WITH_OUTPUT_CAPTURE = 30;

    private AudioStreamMain() {
    }

    public static void main(String[] arguments) {
        // First thing, before any work: the PC times VM start-up and the
        // set-up below separately. See HostSignal.
        HostSignal.alive();
        CaptureSession capture = null;
        String sessionToken = arguments.length > 0 ? arguments[0] : "unknown";
        HostSignal.logUncaught(LOG_TAG, sessionToken);
        try {
            Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO);
            try {
                capture = openCapture(sessionToken);
            } catch (UnsupportedOperationException unsupported) {
                reportUnsupported(unsupported, sessionToken);
                return;
            }
            AudioRecord recorder = capture.recorder;
            if (recorder.getState() != AudioRecord.STATE_INITIALIZED) {
                throw new IllegalStateException("AudioRecord initialization failed");
            }
            recorder.startRecording();
            if (recorder.getRecordingState() != AudioRecord.RECORDSTATE_RECORDING) {
                throw new IllegalStateException("AudioRecord did not start");
            }
            Log.i(
                    LOG_TAG,
                    sessionToken
                            + " start mode="
                            + capture.mode
                            + " sample_rate="
                            + SAMPLE_RATE
                            + " keep_playing_on_device="
                            + capture.keepsPlayingOnDevice);

            // Same localabstract transport as the video and control channels;
            // stdout only carries the ready byte that tells the host the
            // socket is listening.
            String socketName = SOCKET_PREFIX + sessionToken;
            try (LocalServerSocket server = new LocalServerSocket(socketName)) {
                FileOutputStream ready = new FileOutputStream(FileDescriptor.out);
                ready.write(READY_BYTE);
                ready.flush();
                Log.i(LOG_TAG, sessionToken + " listening socket=" + socketName);
                try (LocalSocket client = HostSignal.acceptFromHost(server, LOG_TAG, sessionToken)) {
                    OutputStream output = client.getOutputStream();
                    byte[] buffer = new byte[4096];
                    long reportStartedNs = System.nanoTime();
                    long intervalBytes = 0;
                    long intervalReads = 0;
                    long maxWriteUs = 0;
                    while (true) {
                        int count = recorder.read(
                                buffer,
                                0,
                                buffer.length,
                                AudioRecord.READ_BLOCKING);
                        if (count < 0) {
                            throw new IllegalStateException(
                                    "AudioRecord.read returned " + count);
                        }
                        if (count > 0) {
                            long beforeWriteNs = System.nanoTime();
                            output.write(buffer, 0, count);
                            maxWriteUs = Math.max(
                                    maxWriteUs,
                                    (System.nanoTime() - beforeWriteNs) / 1_000);
                            intervalBytes += count;
                            intervalReads++;
                            long nowNs = System.nanoTime();
                            if (nowNs - reportStartedNs >= 1_000_000_000L) {
                                Log.i(
                                        LOG_TAG,
                                        sessionToken
                                                + " tick bytes="
                                                + intervalBytes
                                                + " reads="
                                                + intervalReads
                                                + " max_write_us="
                                                + maxWriteUs);
                                reportStartedNs = nowNs;
                                intervalBytes = 0;
                                intervalReads = 0;
                                maxWriteUs = 0;
                            }
                        }
                    }
                }
            }
        } catch (Throwable error) {
            Log.e(LOG_TAG, sessionToken + " fatal " + error, error);
        } finally {
            if (capture != null) {
                capture.close(sessionToken);
            }
        }
    }

    /**
     * The capture this Android lets a shell process have, best first.
     *
     * What decides it is the permissions the shell package holds on each
     * release, not the API surface: the duplicating mix needs
     * MODIFY_AUDIO_ROUTING (shell holds it from Android 13), a plain capture
     * of the output mix needs RECORD_AUDIO and CAPTURE_AUDIO_OUTPUT (from
     * Android 11), and before that a shell process has no way in at all. A
     * refusal of the first must not kill the agent: the host would take it for
     * a dropped link and relaunch it every few seconds.
     */
    private static CaptureSession openCapture(String sessionToken) throws Exception {
        List<String> failures = new ArrayList<>();
        if (Build.VERSION.SDK_INT >= FIRST_API_WITH_DUPLICATE) {
            try {
                return createDuplicatingCapture();
            } catch (Throwable error) {
                Log.w(LOG_TAG, sessionToken + " duplicating capture refused: " + summarize(error));
                failures.add("duplicate: " + summarize(error));
            }
        } else {
            failures.add("duplicate: needs Android 13");
        }
        if (Build.VERSION.SDK_INT >= FIRST_API_WITH_OUTPUT_CAPTURE) {
            try {
                return createOutputCapture();
            } catch (Throwable error) {
                Log.w(LOG_TAG, sessionToken + " output capture refused: " + summarize(error));
                failures.add("output: " + summarize(error));
            }
        } else {
            failures.add("output: needs Android 11");
        }
        throw new UnsupportedOperationException(
                "Android " + Build.VERSION.RELEASE + " (API " + Build.VERSION.SDK_INT
                        + ") gives a shell process no way to capture audio — "
                        + join(failures));
    }

    /**
     * Captures the output mix itself (REMOTE_SUBMIX). Android routes the mix
     * to the capture and the phone's own speaker goes quiet while it runs -
     * the sound comes out of the PC instead. It is what Android 11 and 12
     * offer a shell process; the duplicating mix that keeps the phone playing
     * too is Android 13's.
     */
    private static CaptureSession createOutputCapture() throws Exception {
        // AudioRecord looks its calling package up through ActivityThread, and
        // a bare app_process has none: lend it the shell's identity first,
        // exactly as the duplicating capture does.
        acquireAudioCaptureIdentity();
        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();
        int minimum = AudioRecord.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        // A tenth of a second of stereo 16-bit, or whatever the device
        // insists on if that is more.
        int bufferSize = Math.max(minimum, SAMPLE_RATE * 2 * 2 / 10);
        AudioRecord recorder = new AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.REMOTE_SUBMIX)
                .setAudioFormat(format)
                .setBufferSizeInBytes(bufferSize)
                .build();
        return new CaptureSession(recorder, null, null, "remote_submix", false);
    }

    /**
     * Tells the host, on stdout, that audio is not available on this phone
     * and why. The host reads one byte to learn whether the socket is ready;
     * this is the other byte, and the line after it goes into the log.
     */
    private static void reportUnsupported(Throwable error, String sessionToken) {
        String reason = error.getMessage() != null ? error.getMessage() : error.toString();
        // Deliberately NOT " fatal ": the host's diagnostics count that word as
        // a crash, and an Android that simply has no audio path for the shell
        // is an expected outcome, reported to the host over stdout, not a fault.
        Log.w(LOG_TAG, sessionToken + " unsupported: " + reason);
        try {
            FileOutputStream out = new FileOutputStream(FileDescriptor.out);
            out.write(UNSUPPORTED_BYTE);
            out.write(reason.replace('\n', ' ').getBytes("UTF-8"));
            out.write('\n');
            out.flush();
        } catch (Throwable ignored) {
        }
    }

    private static String summarize(Throwable error) {
        Throwable cause = error;
        if (error instanceof java.lang.reflect.InvocationTargetException
                && error.getCause() != null) {
            cause = error.getCause();
        }
        String message = cause.getMessage();
        String name = cause.getClass().getSimpleName();
        if (message == null || message.isEmpty()) {
            return name;
        }
        int newline = message.indexOf('\n');
        if (newline > 0) {
            message = message.substring(0, newline);
        }
        return name + ": " + message;
    }

    private static String join(List<String> parts) {
        StringBuilder text = new StringBuilder();
        for (String part : parts) {
            if (text.length() > 0) {
                text.append("; ");
            }
            text.append(part);
        }
        return text.toString();
    }

    private static CaptureSession createDuplicatingCapture() throws Exception {
        if (Build.VERSION.SDK_INT < FIRST_API_WITH_DUPLICATE) {
            throw new UnsupportedOperationException(
                    "Simultaneous device and PC audio requires Android 13 or newer");
        }

        Class<?> mixingRuleClass =
                Class.forName("android.media.audiopolicy.AudioMixingRule");
        Class<?> mixingRuleBuilderClass =
                Class.forName("android.media.audiopolicy.AudioMixingRule$Builder");
        Object ruleBuilder = mixingRuleBuilderClass.getConstructor().newInstance();
        int playersRole = mixingRuleClass.getField("MIX_ROLE_PLAYERS").getInt(null);
        mixingRuleBuilderClass
                .getMethod("setTargetMixRole", int.class)
                .invoke(ruleBuilder, playersRole);

        AudioAttributes mediaAttributes = new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .build();
        int matchUsage = mixingRuleClass
                .getField("RULE_MATCH_ATTRIBUTE_USAGE")
                .getInt(null);
        mixingRuleBuilderClass
                .getMethod("addMixRule", int.class, Object.class)
                .invoke(ruleBuilder, matchUsage, mediaAttributes);
        Object mixingRule = mixingRuleBuilderClass
                .getMethod("build")
                .invoke(ruleBuilder);

        Class<?> audioMixClass = Class.forName("android.media.audiopolicy.AudioMix");
        Class<?> audioMixBuilderClass =
                Class.forName("android.media.audiopolicy.AudioMix$Builder");
        Object mixBuilder = audioMixBuilderClass
                .getConstructor(mixingRuleClass)
                .newInstance(mixingRule);
        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();
        audioMixBuilderClass
                .getMethod("setFormat", AudioFormat.class)
                .invoke(mixBuilder, format);
        int loopBackRender = audioMixClass
                .getField("ROUTE_FLAG_LOOP_BACK_RENDER")
                .getInt(null);
        audioMixBuilderClass
                .getMethod("setRouteFlags", int.class)
                .invoke(mixBuilder, loopBackRender);
        Object audioMix = audioMixBuilderClass
                .getMethod("build")
                .invoke(mixBuilder);

        Class<?> audioPolicyClass =
                Class.forName("android.media.audiopolicy.AudioPolicy");
        Class<?> audioPolicyBuilderClass =
                Class.forName("android.media.audiopolicy.AudioPolicy$Builder");
        Object policyBuilder = audioPolicyBuilderClass
                .getConstructor(Context.class)
                .newInstance(acquireAudioCaptureIdentity());
        audioPolicyBuilderClass
                .getMethod("addMix", audioMixClass)
                .invoke(policyBuilder, audioMix);
        Object audioPolicy = audioPolicyBuilderClass
                .getMethod("build")
                .invoke(policyBuilder);

        Method register = AudioManager.class.getDeclaredMethod(
                "registerAudioPolicyStatic",
                audioPolicyClass);
        register.setAccessible(true);
        int registrationResult = (Integer) register.invoke(null, audioPolicy);
        if (registrationResult != 0) {
            throw new IllegalStateException(
                    "AudioManager.registerAudioPolicyStatic() rejected the "
                            + "capture policy (result code "
                            + registrationResult
                            + ")");
        }

        Method createSink = audioPolicyClass.getMethod(
                "createAudioRecordSink",
                audioMixClass);
        AudioRecord recorder = (AudioRecord) createSink.invoke(audioPolicy, audioMix);
        if (recorder == null) {
            unregisterAudioPolicy(audioPolicyClass, audioPolicy);
            throw new IllegalStateException("Audio policy did not create a record sink");
        }
        return new CaptureSession(recorder, audioPolicyClass, audioPolicy, "loop_back_render", true);
    }

    private static void unregisterAudioPolicy(
            Class<?> audioPolicyClass,
            Object audioPolicy)
            throws Exception {
        Method unregister = AudioManager.class.getDeclaredMethod(
                "unregisterAudioPolicyAsyncStatic",
                audioPolicyClass);
        unregister.setAccessible(true);
        unregister.invoke(null, audioPolicy);
    }

    /**
     * AudioPolicy.Builder's constructor calls straight into
     * AudioManager.registerAudioPolicyStatic(), and that hidden entry point
     * checks the caller's context against a short allowlist of privileged
     * identities - "com.android.shell" among them - before it will let a
     * mix be registered at all. A plain app Context fails that check, so
     * this process (already running as the shell UID over adb) has to hand
     * AudioPolicy a Context that CLAIMS the shell's package identity for
     * the one call that needs it.
     *
     * There is no public constructor for that: ActivityThread only exposes
     * its live Application through a package-private field, and the only
     * documented way to get an Application instance at all outside of
     * Android's own startup path is Instrumentation.newApplication(). So
     * two things have to happen, in order: build an Application backed by
     * our stand-in Context, then reach into ActivityThread and swap it in
     * as "the" application for this process, because AudioPolicy.Builder
     * reads that shared reference rather than take a Context argument
     * this deep in its own constructor chain.
     */
    private static Context acquireAudioCaptureIdentity() throws Exception {
        ensureLooperPrepared();

        Class<?> activityThreadClass = Class.forName("android.app.ActivityThread");
        Object activityThread =
                activityThreadClass.getDeclaredMethod("systemMain").invoke(null);
        Context platformContext = (Context) activityThreadClass
                .getDeclaredMethod("getSystemContext")
                .invoke(activityThread);

        // AttributionSource exists from API 31; the class that overrides it is
        // only ever loaded there, so Android 11 never resolves the type.
        Context borrowedShellIdentity = Build.VERSION.SDK_INT >= 31
                ? new AudioCaptureIdentity(platformContext)
                : new ShellIdentity(platformContext);
        installAsProcessApplication(activityThreadClass, activityThread, borrowedShellIdentity);
        return borrowedShellIdentity;
    }

    private static void ensureLooperPrepared() {
        if (Looper.myLooper() == null) {
            Looper.prepare();
        }
    }

    private static void installAsProcessApplication(
            Class<?> activityThreadClass,
            Object activityThread,
            Context baseContext)
            throws Exception {
        Application standInApplication =
                Instrumentation.newApplication(Application.class, baseContext);
        Field liveApplicationField =
                activityThreadClass.getDeclaredField("mInitialApplication");
        liveApplicationField.setAccessible(true);
        liveApplicationField.set(activityThread, standInApplication);
    }

    /**
     * A Context wrapper that reports itself as "com.android.shell" - the
     * one non-app identity AudioManager's hidden audio-policy check will
     * accept - while delegating everything else to the real platform
     * Context it wraps. Only the four accessors AudioPolicy.Builder and
     * AudioManager actually consult are overridden; everything else falls
     * through to ContextWrapper's default delegation.
     */
    private static class ShellIdentity extends ContextWrapper {
        static final String SHELL_PACKAGE_NAME = "com.android.shell";

        ShellIdentity(Context base) {
            super(base);
        }

        @Override
        public Context getApplicationContext() {
            return this;
        }

        @Override
        public String getOpPackageName() {
            return SHELL_PACKAGE_NAME;
        }

        @Override
        public String getPackageName() {
            return SHELL_PACKAGE_NAME;
        }
    }

    /** The same identity, with the attribution that API 31+ checks as well. */
    private static final class AudioCaptureIdentity extends ShellIdentity {
        AudioCaptureIdentity(Context base) {
            super(base);
        }

        @Override
        public AttributionSource getAttributionSource() {
            AttributionSource.Builder attributionBuilder =
                    new AttributionSource.Builder(Process.SHELL_UID);
            attributionBuilder.setPackageName(SHELL_PACKAGE_NAME);
            return attributionBuilder.build();
        }
    }

    private static final class CaptureSession {
        final AudioRecord recorder;
        /** Null for a capture that registered no audio policy. */
        final Class<?> policyClass;
        final Object policy;
        /** For the log: which capture this is. */
        final String mode;
        /** Whether the phone's own speaker keeps playing while this runs. */
        final boolean keepsPlayingOnDevice;

        CaptureSession(
                AudioRecord recorder,
                Class<?> policyClass,
                Object policy,
                String mode,
                boolean keepsPlayingOnDevice) {
            this.recorder = recorder;
            this.policyClass = policyClass;
            this.policy = policy;
            this.mode = mode;
            this.keepsPlayingOnDevice = keepsPlayingOnDevice;
        }

        void close(String sessionToken) {
            try {
                recorder.release();
            } catch (Throwable error) {
                Log.w(LOG_TAG, sessionToken + " recorder_release_failed " + error);
            }
            if (policy == null) {
                return;
            }
            try {
                unregisterAudioPolicy(policyClass, policy);
                Log.i(LOG_TAG, sessionToken + " policy_unregistered");
            } catch (Throwable error) {
                Log.w(LOG_TAG, sessionToken + " policy_unregister_failed " + error);
            }
        }
    }
}
