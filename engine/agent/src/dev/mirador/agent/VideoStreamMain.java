package dev.mirador.agent;

import android.content.Context;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaCodecList;
import android.media.MediaFormat;
import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Bundle;
import android.os.Looper;
import android.util.Log;
import android.util.Range;
import android.view.Surface;

import java.io.DataOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.List;

public final class VideoStreamMain {
    private static final String LOG_TAG = "MirrylVideo";
    private static final String SOCKET_PREFIX = "mirryl_video_";
    static final int DISPLAY_ID = 0;
    private static final int VIDEO_MAGIC = 0x4d595631; // MYV1
    private static final int VIDEO_VERSION = 1;
    private static final int VIDEO_HEADER_SIZE = 24;
    private static final int FLAG_CONFIGURATION = 1;
    private static final int FLAG_KEYFRAME = 2;
    private static final String KEY_MAX_FPS_TO_ENCODER = "max-fps-to-encoder";

    /** How often the watcher below asks the real display for its size. */
    private static final long ROTATION_POLL_MS = 400L;

    /** Same size, tried again, for an encoder that says its failure was momentary. */
    private static final int TRANSIENT_RETRIES = 3;
    private static final long TRANSIENT_RETRY_DELAY_MS = 120L;

    /**
     * Set by the rotation watcher to ask the capture loop to rebuild itself.
     *
     * The virtual display is created with a fixed width and height, and Android
     * keeps honouring them when the phone turns: it composes the rotated screen
     * SCALED DOWN AND SIDEWAYS inside the frame it was given. That is why a
     * phone turned to landscape arrived as a small tilted picture no matter what
     * the PC side did - the damage was already done here.
     */
    private static volatile boolean restartRequested;

    /** The encoder the command reader should poke, across rebuilds. */
    private static final java.util.concurrent.atomic.AtomicReference<MediaCodec>
            ACTIVE_ENCODER = new java.util.concurrent.atomic.AtomicReference<>();

    /**
     * The long edge the encoder accepted the last time, or 0 before the first
     * success. A rotation rebuild asks for the turned size clamped to this, so
     * a phone that needed the ladder once does not climb it again on every
     * turn: the answer is a property of the encoder, not of the orientation.
     */
    private static volatile int acceptedLongEdge;

    /**
     * The encoder that took a size the last time, or null before the first
     * success. Same reason as {@link #acceptedLongEdge}: a rotation rebuild
     * starts with it instead of re-trying the ones that refused.
     */
    private static volatile String acceptedEncoderName;

    private VideoStreamMain() {
    }

    /** A started encoder and the surface the mirror paints into. */
    private static final class Capture {
        final MediaCodec encoder;
        final Surface surface;
        final int width;
        final int height;
        final String encoderName;

        Capture(MediaCodec encoder, Surface surface, int width, int height) {
            this.encoder = encoder;
            this.surface = surface;
            this.width = width;
            this.height = height;
            this.encoderName = encoder.getName();
        }

        void release() {
            surface.release();
            stopEncoder(encoder);
        }
    }

    public static void main(String[] arguments) {
        // Before anything else, even argument parsing: the PC is timing how
        // long the VM takes to get here, separately from how long the set-up
        // below takes. See HostSignal.
        HostSignal.alive();
        Capture capture = null;
        MirrorDisplay virtualDisplay = null;
        String sessionToken = "unknown";
        try {
            if (arguments.length != 4) {
                HostSignal.unsupported("the video agent was started with the wrong arguments");
                return;
            }
            int width = parsePositive(arguments[0], 4096);
            int height = parsePositive(arguments[1], 4096);
            int bitRate = parsePositive(arguments[2], 50_000_000);
            sessionToken = arguments[3];
            HostSignal.logUncaught(LOG_TAG, sessionToken);
            if (Looper.myLooper() == null) {
                Looper.prepareMainLooper();
            }

            // The host works the size out from `wm size`, which reports the
            // PHYSICAL screen and so always comes back portrait-shaped. Match
            // what the display is doing right now, so opening a mirror on an
            // already-turned phone starts correct instead of starting sideways.
            int[] initial = orientedSize(width, height, currentDisplaySize());
            width = initial[0];
            height = initial[1];

            // Everything that can turn out impossible on this phone happens
            // here, BEFORE telling the host the socket is ready: the context,
            // the encoder at a size this phone accepts, and the mirror through
            // whichever door this Android leaves open. Whatever fails, the
            // reason goes to the host on stdout while it is still listening
            // there - once the ready byte is out, the host only reads the
            // socket, and a death there looks like a dropped link worth
            // retrying. A phone that cannot do this has to be told once, in
            // words, not relaunched for ever.
            Context context;
            try {
                context = shellContext();
                capture = startEncoder(width, height, bitRate, sessionToken);
                width = capture.width;
                height = capture.height;
                virtualDisplay = openMirror(context, capture.surface, width, height, sessionToken);
            } catch (Throwable error) {
                if (capture != null) {
                    capture.release();
                    capture = null;
                }
                reportUnsupported(error, sessionToken);
                return;
            }

            // The stream travels over a localabstract socket (the same
            // transport the control channel uses) instead of stdout: stdout
            // crosses the adb shell-protocol relay, which costs extra hops and
            // buffering on every frame. The ready byte on stdout tells the host
            // the socket is listening so it can create the forward and connect.
            String socketName = SOCKET_PREFIX + sessionToken;
            try (LocalServerSocket server = new LocalServerSocket(socketName)) {
                HostSignal.ready();
                Log.i(LOG_TAG, sessionToken + " listening socket=" + socketName);
                try (LocalSocket client = HostSignal.acceptFromHost(server, LOG_TAG, sessionToken)) {
                    OutputStream out = client.getOutputStream();
                    ACTIVE_ENCODER.set(capture.encoder);
                    startCommandReader(client.getInputStream());
                    startRotationWatcher(width, height, sessionToken);
                    // Rebuild loop. Every pass streams until the watcher says the
                    // phone turned, then tears the encoder and the virtual display
                    // down and builds them again at the new shape. The SOCKET
                    // survives: the PC side sees a fresh codec config followed by
                    // a keyframe, which is the same thing it already handles after
                    // a reconnect, and resizes its window off the new frame size.
                    while (true) {
                        if (virtualDisplay == null) {
                            virtualDisplay = openMirror(
                                    context,
                                    capture.surface,
                                    width,
                                    height,
                                    sessionToken);
                        }
                        Log.i(
                                LOG_TAG,
                                sessionToken
                                        + " start encoder="
                                        + capture.encoderName
                                        + " width="
                                        + width
                                        + " height="
                                        + height
                                        + " bitrate="
                                        + bitRate);
                        writeEncoderOutput(capture.encoder, out, sessionToken);
                        // Only a rotation gets us here; anything else threw.
                        int[] next = orientedSize(width, height, currentDisplaySize());
                        Log.i(
                                LOG_TAG,
                                sessionToken
                                        + " rotation rebuild "
                                        + width + "x" + height
                                        + " -> "
                                        + next[0] + "x" + next[1]);
                        releaseVirtualDisplay(virtualDisplay);
                        virtualDisplay = null;
                        capture.release();
                        capture = null;
                        capture = startEncoder(next[0], next[1], bitRate, sessionToken);
                        width = capture.width;
                        height = capture.height;
                        ACTIVE_ENCODER.set(capture.encoder);
                        restartRequested = false;
                    }
                }
            }
        } catch (Throwable error) {
            // The host normally discards stderr. Keeping the diagnostic here makes
            // device-specific Android framework changes debuggable over ADB.
            Log.e(LOG_TAG, sessionToken + " fatal " + error, error);
            error.printStackTrace(System.err);
        } finally {
            releaseVirtualDisplay(virtualDisplay);
            if (capture != null) {
                capture.release();
            }
        }
    }

    // ---------- The encoder, at a size this phone accepts ----------

    /**
     * Creates and starts an H.264 encoder at the largest size, at or below the
     * one asked for, that this phone's encoder accepts.
     *
     * The sizes are offered in the order {@link SizeLadder#candidates} gives
     * them - largest picture first, every size RAW before the same size fitted
     * to what the encoder declares. The reason is in that class: an encoder can
     * declare less than it takes (a 1080x2160 phone whose encoder declares
     * heights up to 1088 yet accepts 960x1920), and trusting the declaration
     * first gives half the picture for nothing. The first candidate is always
     * exactly what was asked for, rounded to even numbers.
     *
     * A failure the encoder itself calls momentary is retried at the same size
     * a few times first. When nothing works the exception carries every size
     * tried and why it failed; that text is what the person sees.
     *
     * And the ladder is climbed once per H.264 encoder the phone has, in the
     * order {@link EncoderOrder} gives: the system's first encoder comes first;
     * only when it refuses the whole ladder does the next one get its turn.
     */
    private static Capture startEncoder(
            int requestedWidth,
            int requestedHeight,
            int bitRate,
            String sessionToken)
            throws Exception {
        int[] requested = clampToAccepted(requestedWidth & ~1, requestedHeight & ~1);
        List<String> names = avcEncoderNames(sessionToken);
        List<String> refusedByHook =
                EncoderOrder.parseList(System.getenv("MIRRYL_ENCODER_REFUSE"));
        if (names.isEmpty()) {
            // The list could not be read. Let the system pick, as every
            // release before this one did.
            names.add(null);
        }
        List<String> failures = new ArrayList<>();
        int[] created = {0};
        for (int i = 0; i < names.size(); i++) {
            String name = names.get(i);
            Capture capture = startEncoderNamed(
                    name,
                    i > 0,
                    requested,
                    requestedWidth,
                    requestedHeight,
                    bitRate,
                    refusedByHook,
                    failures,
                    created,
                    sessionToken);
            if (capture != null) {
                acceptedEncoderName = capture.encoderName;
                return capture;
            }
        }
        if (created[0] == 0) {
            throw new UnsupportedOperationException(
                    "this phone has no H.264 video encoder (" + join(failures) + ")");
        }
        if (names.size() == 1) {
            // One encoder: the words every earlier release used.
            throw new UnsupportedOperationException(
                    "the video encoder refused every size tried - " + join(failures));
        }
        throw new UnsupportedOperationException(
                "none of this phone's " + names.size()
                        + " H.264 encoders took any size tried - " + join(failures));
    }

    /**
     * Climbs the size ladder on one encoder. Returns the started capture, or
     * null after adding to `failures` what this encoder refused; a null name
     * means "whatever the system picks". A fallback encoder - any but the
     * first - gets the bit rate lowered to what it declares, because software
     * encoders cap low (2 Mbit/s on Google's AVC) and may refuse more.
     */
    private static Capture startEncoderNamed(
            String name,
            boolean fallback,
            int[] requested,
            int requestedWidth,
            int requestedHeight,
            int bitRate,
            List<String> refusedByHook,
            List<String> failures,
            int[] created,
            String sessionToken) {
        String label = name == null ? "default" : name;
        MediaCodecInfo.VideoCapabilities capabilities = null;
        List<int[]> candidates = null;
        List<String> refused = new ArrayList<>();
        int index = 0;
        int transientRetriesLeft = TRANSIENT_RETRIES;
        int[] size = requested;
        int encoderBitRate = bitRate;
        while (true) {
            MediaCodec encoder = null;
            try {
                encoder = name == null
                        ? MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
                        : MediaCodec.createByCodecName(name);
            } catch (Throwable error) {
                failures.add(label + ": could not be created (" + MirrorDisplay.summarize(error) + ")");
                Log.w(LOG_TAG, sessionToken + " encoder " + label + " could not be created: "
                        + MirrorDisplay.summarize(error));
                return null;
            }
            created[0]++;
            try {
                if (candidates == null) {
                    capabilities = readCapabilities(encoder);
                    candidates = SizeLadder.candidates(
                            requested[0], requested[1], fitter(capabilities));
                    if (capabilities != null) {
                        Log.i(
                                LOG_TAG,
                                sessionToken + " encoder " + encoder.getName() + " declares widths "
                                        + capabilities.getSupportedWidths() + " heights "
                                        + capabilities.getSupportedHeights() + " alignment "
                                        + capabilities.getWidthAlignment() + "x"
                                        + capabilities.getHeightAlignment());
                        if (fallback) {
                            int upper = capabilities.getBitrateRange().getUpper();
                            if (upper > 0 && upper < encoderBitRate) {
                                Log.i(LOG_TAG, sessionToken + " encoder " + encoder.getName()
                                        + " bitrate " + encoderBitRate + " -> " + upper);
                                encoderBitRate = upper;
                            }
                        }
                    }
                }
                if (refusedByHook.contains(encoder.getName())) {
                    throw new IllegalArgumentException("refused by MIRRYL_ENCODER_REFUSE");
                }
                encoder.configure(
                        videoFormat(size[0], size[1], encoderBitRate),
                        null,
                        null,
                        MediaCodec.CONFIGURE_FLAG_ENCODE);
                Surface surface = encoder.createInputSurface();
                encoder.start();
                requestSyncFrame(encoder);
                acceptedLongEdge = Math.max(size[0], size[1]);
                if (size[0] != requestedWidth || size[1] != requestedHeight) {
                    Log.i(
                            LOG_TAG,
                            sessionToken + " encoder size " + requestedWidth + "x"
                                    + requestedHeight + " -> " + size[0] + "x" + size[1]);
                }
                return new Capture(encoder, surface, size[0], size[1]);
            } catch (Throwable error) {
                stopEncoder(encoder);
                String reason = size[0] + "x" + size[1] + ": " + MirrorDisplay.summarize(error);
                Log.w(LOG_TAG, sessionToken + " encoder " + label + " refused " + reason);
                if (isTransient(error) && transientRetriesLeft-- > 0) {
                    try {
                        Thread.sleep(TRANSIENT_RETRY_DELAY_MS);
                    } catch (InterruptedException interrupted) {
                        Thread.currentThread().interrupt();
                    }
                    continue;
                }
                refused.add(reason);
            }
            // Next size to try. `candidates` is set before the first configure;
            // if reading the capabilities itself threw, fall back to the ladder
            // without a fitter rather than give up.
            if (candidates == null) {
                candidates = SizeLadder.candidates(requested[0], requested[1], null);
            }
            if (++index >= candidates.size()) {
                failures.add(name == null ? join(refused) : label + ": " + join(refused));
                return null;
            }
            size = candidates.get(index);
        }
    }

    /**
     * The phone's H.264 encoders in the order to try them, logged once per
     * start. Empty when the list cannot be read, which the caller takes as
     * "let the system pick".
     */
    private static List<String> avcEncoderNames(String sessionToken) {
        // Test hooks, like MIRRYL_MIRROR_DOORS: `MIRRYL_ENCODERS` is the list
        // to try, in order, instead of the one worked out below (aliases
        // included - that is how the fallback is exercised on an emulator,
        // whose only real AVC encoder is the software one), and
        // `MIRRYL_ENCODER_REFUSE` names encoders to treat as refusing every
        // size. Neither is set in normal use.
        List<String> forced = EncoderOrder.parseList(System.getenv("MIRRYL_ENCODERS"));
        if (!forced.isEmpty()) {
            Log.i(LOG_TAG, sessionToken + " encoders to try (MIRRYL_ENCODERS): " + join(forced));
            return forced;
        }
        List<EncoderOrder.Entry> entries = new ArrayList<>();
        try {
            MediaCodecInfo[] infos = new MediaCodecList(MediaCodecList.REGULAR_CODECS).getCodecInfos();
            for (MediaCodecInfo info : infos) {
                if (!info.isEncoder() || !supportsAvc(info)) {
                    continue;
                }
                boolean software;
                boolean alias = false;
                if (android.os.Build.VERSION.SDK_INT >= 29) {
                    software = info.isSoftwareOnly();
                    alias = info.isAlias();
                } else {
                    software = EncoderOrder.looksSoftware(info.getName());
                }
                entries.add(new EncoderOrder.Entry(info.getName(), software, alias));
            }
        } catch (Throwable error) {
            Log.w(LOG_TAG, sessionToken + " encoder list unreadable: " + MirrorDisplay.summarize(error));
            return new ArrayList<>();
        }
        List<String> order = EncoderOrder.order(entries, acceptedEncoderName);
        Log.i(LOG_TAG, sessionToken + " encoders to try: " + join(order));
        return order;
    }

    private static boolean supportsAvc(MediaCodecInfo info) {
        for (String type : info.getSupportedTypes()) {
            if (MediaFormat.MIMETYPE_VIDEO_AVC.equalsIgnoreCase(type)) {
                return true;
            }
        }
        return false;
    }

    /** The declared limits as a fitter for the ladder, or null when the encoder declares nothing. */
    private static SizeLadder.Fitter fitter(final MediaCodecInfo.VideoCapabilities capabilities) {
        if (capabilities == null) {
            return null;
        }
        return new SizeLadder.Fitter() {
            @Override
            public int[] fit(int width, int height) {
                return fitToCapabilities(width, height, capabilities);
            }

            @Override
            public int[] fitEitherWay(int width, int height) {
                return fitToCapabilitiesEitherWay(width, height, capabilities);
            }
        };
    }

    /** The format every release so far has used; the size is the only thing that varies. */
    private static MediaFormat videoFormat(int width, int height, int bitRate) {
        MediaFormat format = MediaFormat.createVideoFormat(
                MediaFormat.MIMETYPE_VIDEO_AVC,
                width,
                height);
        format.setInteger(
                MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE, bitRate);
        format.setInteger(MediaFormat.KEY_FRAME_RATE, 60);
        // KEY_FRAME_RATE is descriptive for surface-input encoders on some
        // Qualcomm devices. This capture-specific key actually throttles a
        // 120 Hz display before it overloads the decoder and compositor.
        format.setFloat(KEY_MAX_FPS_TO_ENCODER, 60.0f);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 10);
        format.setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 100_000L);
        format.setInteger(MediaFormat.KEY_PRIORITY, 0);
        format.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0);
        if (android.os.Build.VERSION.SDK_INT >= 26) {
            format.setInteger(MediaFormat.KEY_LATENCY, 1);
        }
        return format;
    }

    /** What the encoder declares about sizes, or null if it will not say. */
    private static MediaCodecInfo.VideoCapabilities readCapabilities(MediaCodec encoder) {
        try {
            return encoder
                    .getCodecInfo()
                    .getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
                    .getVideoCapabilities();
        } catch (Throwable ignored) {
            return null;
        }
    }

    /**
     * Whether the encoder itself says this failure may not happen again. Only
     * a CodecException knows; anything else is taken at its word.
     */
    private static boolean isTransient(Throwable error) {
        if (!(error instanceof MediaCodec.CodecException)) {
            return false;
        }
        MediaCodec.CodecException codecError = (MediaCodec.CodecException) error;
        return codecError.isTransient() || codecError.isRecoverable();
    }

    /**
     * The size, shrunk and aligned until the encoder's declared capabilities
     * accept it. Keeps the aspect ratio; never grows.
     */
    static int[] fitToCapabilities(int width, int height, MediaCodecInfo.VideoCapabilities caps) {
        int alignment = Math.max(2, Math.max(caps.getWidthAlignment(), caps.getHeightAlignment()));
        Range<Integer> widths = caps.getSupportedWidths();
        Range<Integer> heights = caps.getSupportedHeights();
        double scale = Math.min(
                1.0,
                Math.min(
                        widths.getUpper() / (double) width,
                        heights.getUpper() / (double) height));
        int fittedWidth = alignDown((int) Math.floor(width * scale), alignment);
        int fittedHeight = alignDown((int) Math.floor(height * scale), alignment);
        // Some encoders bound one side by the other (a total-pixel or
        // block-count limit); shrink a little at a time until the pair is
        // accepted, or give up and let `configure` have the last word.
        for (int step = 0; step < 8; step++) {
            if (fittedWidth < alignment || fittedHeight < alignment) {
                break;
            }
            if (caps.isSizeSupported(fittedWidth, fittedHeight)) {
                break;
            }
            fittedWidth = alignDown((int) Math.floor(fittedWidth * 0.9), alignment);
            fittedHeight = alignDown((int) Math.floor(fittedHeight * 0.9), alignment);
        }
        return new int[] {
            Math.max(fittedWidth, alignment),
            Math.max(fittedHeight, alignment),
        };
    }

    /**
     * The size shrunk, aspect kept, until its LONG side fits the larger of the
     * two declared maxima and its SHORT side the smaller - as if the encoder
     * could be turned on its side. Measured (see SizeLadder): a portrait
     * phone whose encoder declares 1920x1088 takes 960x1920, which is
     * exactly this size, while {@link #fitToCapabilities} only offers 544x1088.
     * Aligned like the other fit; never grows.
     */
    static int[] fitToCapabilitiesEitherWay(
            int width, int height, MediaCodecInfo.VideoCapabilities caps) {
        int alignment = Math.max(2, Math.max(caps.getWidthAlignment(), caps.getHeightAlignment()));
        int larger = Math.max(caps.getSupportedWidths().getUpper(), caps.getSupportedHeights().getUpper());
        int smaller = Math.min(caps.getSupportedWidths().getUpper(), caps.getSupportedHeights().getUpper());
        int longSide = Math.max(width, height);
        int shortSide = Math.min(width, height);
        double scale = Math.min(1.0, Math.min(larger / (double) longSide, smaller / (double) shortSide));
        int fittedWidth = alignDown((int) Math.floor(width * scale), alignment);
        int fittedHeight = alignDown((int) Math.floor(height * scale), alignment);
        return new int[] {
            Math.max(fittedWidth, alignment),
            Math.max(fittedHeight, alignment),
        };
    }

    /** The requested aspect ratio at a smaller long edge, in even numbers. */
    static int[] scaleToLongEdge(int width, int height, int longEdge) {
        return SizeLadder.scaleToLongEdge(width, height, longEdge);
    }

    /** The size, with its long edge no larger than the last one the encoder accepted. */
    private static int[] clampToAccepted(int width, int height) {
        int accepted = acceptedLongEdge;
        if (accepted <= 0 || Math.max(width, height) <= accepted) {
            return new int[] {width, height};
        }
        return scaleToLongEdge(width, height, accepted);
    }

    private static int alignDown(int value, int alignment) {
        return SizeLadder.alignDown(value, alignment);
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

    private static void requestSyncFrame(MediaCodec encoder) {
        try {
            Bundle parameters = new Bundle();
            parameters.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
            encoder.setParameters(parameters);
        } catch (Throwable ignored) {
        }
    }

    private static void stopEncoder(MediaCodec encoder) {
        if (encoder == null) {
            return;
        }
        try {
            encoder.stop();
        } catch (Throwable ignored) {
        }
        try {
            encoder.release();
        } catch (Throwable ignored) {
        }
    }

    /**
     * The last display size read successfully, for callers that need one even
     * when a single read fails. The rotation watcher refreshes it constantly.
     */
    private static volatile int[] lastKnownDisplay;

    /** The last display size seen, or null if none has ever been read. */
    static int[] lastDisplaySize() {
        int[] fresh = currentDisplaySize();
        return fresh != null ? fresh : lastKnownDisplay;
    }

    /** The real display's current logical size, or null if it cannot be read. */
    static int[] currentDisplaySize() {
        try {
            Class<?> globalType =
                    Class.forName("android.hardware.display.DisplayManagerGlobal");
            Object global = globalType.getMethod("getInstance").invoke(null);
            Object info = globalType
                    .getMethod("getDisplayInfo", int.class)
                    .invoke(global, DISPLAY_ID);
            if (info == null) {
                return null;
            }
            Class<?> infoType = info.getClass();
            int displayWidth = infoType.getField("logicalWidth").getInt(info);
            int displayHeight = infoType.getField("logicalHeight").getInt(info);
            if (displayWidth > 0 && displayHeight > 0) {
                int[] size = new int[] {displayWidth, displayHeight};
                lastKnownDisplay = size;
                return size;
            }
        } catch (Throwable ignored) {
            // Framework internals move between Android versions. A capture that
            // cannot read the rotation is the OLD behaviour, not a dead session.
        }
        return null;
    }

    /**
     * The capture size to use: the pixel budget the host asked for, turned to
     * match the display. Swapping keeps the exact pixel count, so the bitrate
     * the host chose for it stays right.
     */
    private static int[] orientedSize(int width, int height, int[] display) {
        if (display == null) {
            return new int[] {width, height};
        }
        boolean displayIsWide = display[0] > display[1];
        boolean captureIsWide = width > height;
        if (displayIsWide == captureIsWide) {
            return new int[] {width, height};
        }
        return new int[] {height, width};
    }

    /**
     * Watches the real display and asks for a rebuild when it turns.
     *
     * Polled rather than event-driven on purpose: a shell process has no
     * Activity to receive configuration changes, and a DisplayListener wants a
     * Looper thread this agent does not otherwise need. Twice a second costs
     * nothing next to encoding video.
     */
    private static void startRotationWatcher(
            final int startWidth,
            final int startHeight,
            final String sessionToken) {
        Thread watcher = new Thread(new Runnable() {
            @Override
            public void run() {
                boolean captureIsWide = startWidth > startHeight;
                while (true) {
                    try {
                        Thread.sleep(ROTATION_POLL_MS);
                    } catch (InterruptedException interrupted) {
                        return;
                    }
                    if (restartRequested) {
                        continue;
                    }
                    int[] display = currentDisplaySize();
                    if (display == null) {
                        continue;
                    }
                    boolean displayIsWide = display[0] > display[1];
                    if (displayIsWide != captureIsWide) {
                        captureIsWide = displayIsWide;
                        Log.i(
                                LOG_TAG,
                                sessionToken
                                        + " display turned to "
                                        + display[0] + "x" + display[1]);
                        restartRequested = true;
                    }
                }
            }
        }, "mirryl-video-rotation");
        watcher.setDaemon(true);
        watcher.start();
    }

    private static int parsePositive(String value, int maximum) {
        int parsed = Integer.parseInt(value);
        if (parsed <= 0 || parsed > maximum) {
            throw new IllegalArgumentException("numeric argument out of range");
        }
        return parsed;
    }

    private static Context shellContext() throws Exception {
        Class<?> activityThread = Class.forName("android.app.ActivityThread");
        Object thread = activityThread.getDeclaredMethod("systemMain").invoke(null);
        Context system =
                (Context) activityThread.getDeclaredMethod("getSystemContext").invoke(thread);
        return system.createPackageContext(
                "com.android.shell",
                Context.CONTEXT_IGNORE_SECURITY);
    }

    /**
     * The mirror of the real display into the encoder surface, through
     * whichever door this Android leaves open - see {@link MirrorDisplay}.
     */
    private static MirrorDisplay openMirror(
            Context context,
            Surface surface,
            int width,
            int height,
            String sessionToken)
            throws Exception {
        return MirrorDisplay.open(
                context,
                surface,
                width,
                height,
                currentDisplaySize(),
                currentLayerStack(),
                LOG_TAG,
                sessionToken);
    }

    private static void releaseVirtualDisplay(MirrorDisplay display) {
        if (display != null) {
            display.release();
        }
    }

    /**
     * Tells the host, on stdout, that this phone cannot be mirrored and why.
     * The host reads one byte to learn whether the socket is ready; this is
     * the other byte, and the line after it is shown to the person as-is.
     */
    private static void reportUnsupported(Throwable error, String sessionToken) {
        String reason = error.getMessage() != null ? error.getMessage() : error.toString();
        // Not " fatal ": the host counts that word as a crash. A phone that
        // leaves no way to mirror is an expected outcome reported over stdout.
        Log.w(LOG_TAG, sessionToken + " unsupported: " + reason);
        HostSignal.unsupported(reason);
    }

    /**
     * The layer stack the real display composes: what a SurfaceFlinger mirror
     * has to be told to look at. It is 0 on every phone seen so far, but the
     * field is public in DisplayInfo, so read it rather than assume it.
     */
    private static int currentLayerStack() {
        try {
            Class<?> globalType =
                    Class.forName("android.hardware.display.DisplayManagerGlobal");
            Object global = globalType.getMethod("getInstance").invoke(null);
            Object info = globalType
                    .getMethod("getDisplayInfo", int.class)
                    .invoke(global, DISPLAY_ID);
            if (info != null) {
                return info.getClass().getField("layerStack").getInt(info);
            }
        } catch (Throwable ignored) {
        }
        return 0;
    }

    private static void writeEncoderOutput(
            MediaCodec encoder,
            OutputStream output,
            String sessionToken)
            throws Exception {
        DataOutputStream data = new DataOutputStream(output);
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        byte[] transfer = new byte[64 * 1024];
        int sequence = 0;
        long reportStartedNs = System.nanoTime();
        long intervalPackets = 0;
        long intervalBytes = 0;
        long intervalKeyframes = 0;
        long maxEncodeAgeUs = 0;
        long maxWriteUs = 0;
        while (true) {
            if (restartRequested) {
                return;
            }
            // Was an infinite wait. It has to time out now, or a phone that
            // turned while the screen was still would never wake this loop up
            // to notice - the rebuild would wait for the next frame that the
            // rotation itself was supposed to trigger.
            int index = encoder.dequeueOutputBuffer(info, 100_000);
            if (index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                Log.i(LOG_TAG, sessionToken + " format " + encoder.getOutputFormat());
                continue;
            }
            if (index == MediaCodec.INFO_TRY_AGAIN_LATER) {
                continue;
            }
            if (index < 0) {
                continue;
            }
            ByteBuffer buffer = encoder.getOutputBuffer(index);
            if (buffer != null && info.size > 0) {
                int flags = 0;
                if ((info.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0) {
                    flags |= FLAG_CONFIGURATION;
                }
                if ((info.flags & MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0) {
                    flags |= FLAG_KEYFRAME;
                }
                long beforeWriteNs = System.nanoTime();
                if (info.presentationTimeUs > 0) {
                    long encodeAgeUs = beforeWriteNs / 1_000 - info.presentationTimeUs;
                    if (encodeAgeUs >= 0 && encodeAgeUs < 60_000_000) {
                        maxEncodeAgeUs = Math.max(maxEncodeAgeUs, encodeAgeUs);
                    }
                }
                data.writeInt(VIDEO_MAGIC);
                data.writeByte(VIDEO_VERSION);
                data.writeByte(flags);
                data.writeShort(VIDEO_HEADER_SIZE);
                data.writeInt(info.size);
                data.writeInt(sequence++);
                data.writeLong(info.presentationTimeUs);

                buffer.position(info.offset);
                buffer.limit(info.offset + info.size);
                while (buffer.hasRemaining()) {
                    int count = Math.min(buffer.remaining(), transfer.length);
                    buffer.get(transfer, 0, count);
                    data.write(transfer, 0, count);
                }
                data.flush();
                long writeUs = (System.nanoTime() - beforeWriteNs) / 1_000;
                maxWriteUs = Math.max(maxWriteUs, writeUs);
                intervalPackets++;
                intervalBytes += info.size;
                if ((flags & FLAG_KEYFRAME) != 0) {
                    intervalKeyframes++;
                }
                long nowNs = System.nanoTime();
                if (nowNs - reportStartedNs >= 1_000_000_000L) {
                    long intervalMs = Math.max(1, (nowNs - reportStartedNs) / 1_000_000);
                    long fpsTimes10 = intervalPackets * 10_000 / intervalMs;
                    Log.i(
                            LOG_TAG,
                            sessionToken
                                    + " tick packets="
                                    + intervalPackets
                                    + " bytes="
                                    + intervalBytes
                                    + " fps_x10="
                                    + fpsTimes10
                                    + " keyframes="
                                    + intervalKeyframes
                                    + " max_encode_age_us="
                                    + maxEncodeAgeUs
                                    + " max_write_us="
                                    + maxWriteUs
                                    + " pts_us="
                                    + info.presentationTimeUs);
                    reportStartedNs = nowNs;
                    intervalPackets = 0;
                    intervalBytes = 0;
                    intervalKeyframes = 0;
                    maxEncodeAgeUs = 0;
                    maxWriteUs = 0;
                }
            }
            encoder.releaseOutputBuffer(index, false);
        }
    }

    private static void startCommandReader(InputStream input) {
        Thread reader = new Thread(new Runnable() {
            @Override
            public void run() {
                try {
                    while (true) {
                        int command = input.read();
                        if (command < 0) {
                            return;
                        }
                        if (command == 'K') {
                            // Read fresh every time: across a rotation rebuild the
                            // encoder this thread started with is already released,
                            // and poking a released codec crashes the agent.
                            MediaCodec active = ACTIVE_ENCODER.get();
                            if (active != null) {
                                requestSyncFrame(active);
                            }
                        }
                    }
                } catch (Throwable ignored) {
                    // The socket closing is part of normal session shutdown.
                }
            }
        }, "mirryl-video-control");
        reader.setDaemon(true);
        reader.start();
    }
}
