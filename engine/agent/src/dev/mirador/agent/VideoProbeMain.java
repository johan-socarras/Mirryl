package dev.mirador.agent;

import android.content.Context;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Bundle;
import android.os.Looper;
import android.view.Surface;

import java.io.OutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;

public final class VideoProbeMain {
    private static final int WIDTH = 1080;
    private static final int HEIGHT = 2340;
    private static final int BIT_RATE = 8_000_000;
    private static final int FRAME_RATE = 60;
    private static final int DISPLAY_ID = 0;

    private VideoProbeMain() {
    }

    public static void main(String[] arguments) {
        MediaCodec encoder = null;
        Surface inputSurface = null;
        Object virtualDisplay = null;
        try {
            if (Looper.myLooper() == null) {
                Looper.prepareMainLooper();
            }
            MediaFormat format = MediaFormat.createVideoFormat(
                    MediaFormat.MIMETYPE_VIDEO_AVC,
                    WIDTH,
                    HEIGHT);
            format.setInteger(
                    MediaFormat.KEY_COLOR_FORMAT,
                    MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
            format.setInteger(MediaFormat.KEY_BIT_RATE, BIT_RATE);
            format.setInteger(MediaFormat.KEY_FRAME_RATE, FRAME_RATE);
            format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1);
            format.setInteger(MediaFormat.KEY_PRIORITY, 0);
            format.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0);
            if (android.os.Build.VERSION.SDK_INT >= 30) {
                format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1);
            }

            encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC);
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            inputSurface = encoder.createInputSurface();
            encoder.start();

            Bundle parameters = new Bundle();
            parameters.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
            encoder.setParameters(parameters);

            Context context = systemContext();
            virtualDisplay = createMirroringDisplay(context, inputSurface);
            System.err.println(
                    "Mirryl video probe active "
                            + WIDTH
                            + "x"
                            + HEIGHT
                            + " display="
                            + virtualDisplay);

            writeEncoderOutput(encoder, System.out, 5_000_000_000L);
        } catch (Throwable error) {
            error.printStackTrace(System.err);
            System.exit(1);
        } finally {
            releaseVirtualDisplay(virtualDisplay);
            if (inputSurface != null) {
                inputSurface.release();
            }
            if (encoder != null) {
                try {
                    encoder.stop();
                } catch (Throwable ignored) {
                }
                encoder.release();
            }
        }
    }

    private static Context systemContext() throws Exception {
        Class<?> activityThread = Class.forName("android.app.ActivityThread");
        Object thread = activityThread.getDeclaredMethod("systemMain").invoke(null);
        Context system =
                (Context) activityThread.getDeclaredMethod("getSystemContext").invoke(thread);
        return system.createPackageContext(
                "com.android.shell",
                Context.CONTEXT_IGNORE_SECURITY);
    }

    private static Object createMirroringDisplay(Context context, Surface surface)
            throws Exception {
        Class<?> builderType =
                Class.forName("android.hardware.display.VirtualDisplayConfig$Builder");
        Constructor<?> constructor =
                builderType.getConstructor(String.class, int.class, int.class, int.class);
        Object builder = constructor.newInstance("Mirryl Connect", WIDTH, HEIGHT, 420);

        // PUBLIC | AUTO_MIRROR | ROTATES_WITH_CONTENT.
        int flags = 1 | 16 | 128;
        builderType.getMethod("setFlags", int.class).invoke(builder, flags);
        builderType.getMethod("setSurface", Surface.class).invoke(builder, surface);
        builderType.getMethod("setDisplayIdToMirror", int.class).invoke(builder, DISPLAY_ID);
        Object config = builderType.getMethod("build").invoke(builder);

        Class<?> globalType =
                Class.forName("android.hardware.display.DisplayManagerGlobal");
        Object global = globalType.getMethod("getInstance").invoke(null);
        Class<?> configType =
                Class.forName("android.hardware.display.VirtualDisplayConfig");
        Class<?> projectionType =
                Class.forName("android.media.projection.MediaProjection");
        Class<?> callbackType =
                Class.forName("android.hardware.display.VirtualDisplay$Callback");
        Method create = globalType.getMethod(
                "createVirtualDisplay",
                Context.class,
                projectionType,
                configType,
                callbackType,
                java.util.concurrent.Executor.class);
        Object display = create.invoke(global, context, null, config, null, null);
        if (display == null) {
            throw new IllegalStateException("DisplayManager returned no virtual display");
        }
        return display;
    }

    private static void releaseVirtualDisplay(Object display) {
        if (display == null) {
            return;
        }
        try {
            display.getClass().getMethod("release").invoke(display);
        } catch (Throwable error) {
            System.err.println("Unable to release virtual display: " + error);
        }
    }

    private static void writeEncoderOutput(
            MediaCodec encoder,
            OutputStream output,
            long durationNanos)
            throws Exception {
        MediaCodec.BufferInfo info = new MediaCodec.BufferInfo();
        long deadline = System.nanoTime() + durationNanos;
        byte[] transfer = new byte[64 * 1024];
        while (System.nanoTime() < deadline) {
            int index = encoder.dequeueOutputBuffer(info, 50_000);
            if (index == MediaCodec.INFO_TRY_AGAIN_LATER
                    || index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                continue;
            }
            if (index < 0) {
                continue;
            }
            ByteBuffer buffer = encoder.getOutputBuffer(index);
            if (buffer != null && info.size > 0) {
                buffer.position(info.offset);
                buffer.limit(info.offset + info.size);
                while (buffer.hasRemaining()) {
                    int count = Math.min(buffer.remaining(), transfer.length);
                    buffer.get(transfer, 0, count);
                    output.write(transfer, 0, count);
                }
                output.flush();
            }
            encoder.releaseOutputBuffer(index, false);
        }
    }
}
