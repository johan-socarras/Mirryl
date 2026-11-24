package dev.mirador.agent;

import android.content.Context;
import android.graphics.Rect;
import android.os.Build;
import android.os.IBinder;
import android.util.Log;
import android.view.Surface;

import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.List;

/**
 * A display that mirrors the phone's screen into the encoder's surface, opened
 * through whichever framework door this Android release leaves open to a
 * process running as the shell user. There are three, and which one works is
 * a matter of permissions, not of API shape:
 *
 * <ul>
 *   <li><b>The system's own mirroring helper</b>: a static
 *       `DisplayManager.createVirtualDisplay(name, width, height,
 *       displayIdToMirror, surface)` that Android 14 (API 34) added for
 *       exactly this job - a mirror of one display into a surface, created by
 *       a shell process. It builds the config itself, asks the package
 *       manager which package this uid is, and is what the platform keeps
 *       working release after release. It needs CAPTURE_VIDEO_OUTPUT, which
 *       the shell package holds from Android 14 on.</li>
 *   <li><b>Display manager virtual display</b> (`DisplayManagerGlobal
 *       .createVirtualDisplay` with a `VirtualDisplayConfig` and the
 *       AUTO_MIRROR flag): the same thing done by hand, for a build whose
 *       helper is missing or refuses. Same permission, so it is only ever
 *       expected to work on Android 14 and later too. The signature moved
 *       between releases (a Handler on 30, an Executor plus a window Context
 *       on 31-33, an Executor alone on 34+), so it is looked up by parameter
 *       types instead of being spelled out.</li>
 *   <li><b>SurfaceFlinger display</b> (`SurfaceControl.createDisplay` plus a
 *       Transaction that sets its surface, projection and layer stack). Gated
 *       by ACCESS_SURFACE_FLINGER, which the shell has held since at least
 *       Android 9. Android 15 removed `createDisplay`.</li>
 * </ul>
 *
 * All are tried, the ones this release is expected to allow first, so a
 * vendor build that closes one door still gets the others. Only when every
 * door is shut does {@link #open} throw, with every reason in the message -
 * that text travels to the PC and is what the person sees.
 *
 * On the flags: a mirror is a PRIVATE display that auto-mirrors, and nothing
 * more. PUBLIC would make the mirror a real external display in the eyes of
 * every app and of
 * the system - the kind of display a launcher, a desktop mode or a
 * cast feature reacts to, each vendor in its own way. The system helper
 * above, and every screen recorder built on MediaProjection, use a private
 * auto-mirroring display; that is the path every phone exercises daily, so
 * it is the one to be on.
 */
final class MirrorDisplay {
    private static final String DISPLAY_NAME = "Mirryl Connect";
    /** DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, and nothing else - see above. */
    private static final int VIRTUAL_DISPLAY_FLAGS = 16;
    /** The first release whose shell package holds CAPTURE_VIDEO_OUTPUT. */
    private static final int FIRST_API_WITH_SHELL_VIDEO_CAPTURE = 34;

    /** The three ways in, named for the log and the failure message. */
    private enum Door {
        SYSTEM_HELPER("system mirroring helper"),
        DISPLAY_MANAGER("display manager"),
        SURFACE_FLINGER("surfaceflinger");

        final String label;

        Door(String label) {
            this.label = label;
        }
    }

    /** Order of attempts: what this release is expected to allow comes first. */
    private static final Door[] DOORS_MODERN = {
        Door.SYSTEM_HELPER, Door.DISPLAY_MANAGER, Door.SURFACE_FLINGER,
    };
    private static final Door[] DOORS_LEGACY = {
        Door.SURFACE_FLINGER, Door.DISPLAY_MANAGER, Door.SYSTEM_HELPER,
    };

    private final String description;
    private final Object virtualDisplay;
    private final IBinder surfaceFlingerToken;

    private MirrorDisplay(String description, Object virtualDisplay, IBinder token) {
        this.description = description;
        this.virtualDisplay = virtualDisplay;
        this.surfaceFlingerToken = token;
    }

    /** Which door opened, for the log. */
    String describe() {
        return description;
    }

    /**
     * Opens a mirror of the real display into {@code surface}.
     *
     * @param sourceSize the real display's current logical size, or null when
     *     it could not be read; only the SurfaceFlinger door needs it (the
     *     display manager works it out from the mirrored display itself).
     */
    static MirrorDisplay open(
            Context context,
            Surface surface,
            int width,
            int height,
            int[] sourceSize,
            int layerStack,
            String logTag,
            String sessionToken)
            throws Exception {
        List<String> failures = new ArrayList<>();
        Door[] doors = doorsFromEnvironment(
                Build.VERSION.SDK_INT >= FIRST_API_WITH_SHELL_VIDEO_CAPTURE
                        ? DOORS_MODERN
                        : DOORS_LEGACY);
        int flags = flagsFromEnvironment();
        for (Door door : doors) {
            try {
                MirrorDisplay opened;
                switch (door) {
                    case SYSTEM_HELPER:
                        opened = openSystemHelperDisplay(surface, width, height);
                        break;
                    case DISPLAY_MANAGER:
                        opened = openVirtualDisplay(context, surface, width, height, flags);
                        break;
                    default:
                        opened = openSurfaceFlingerDisplay(
                                surface, width, height, sourceSize, layerStack);
                        break;
                }
                Log.i(logTag, sessionToken + " mirror display via " + opened.description);
                return opened;
            } catch (Throwable error) {
                Log.w(logTag, sessionToken + " " + door.label + " door closed: " + summarize(error));
                failures.add(door.label + ": " + summarize(error));
            }
        }
        throw new UnsupportedOperationException(
                "Android " + Build.VERSION.RELEASE + " (API " + Build.VERSION.SDK_INT
                        + ") left no way to mirror the screen - "
                        + join(failures));
    }

    void release() {
        if (virtualDisplay != null) {
            try {
                virtualDisplay.getClass().getMethod("release").invoke(virtualDisplay);
            } catch (Throwable ignored) {
            }
        }
        if (surfaceFlingerToken != null) {
            try {
                Class<?> surfaceControl = Class.forName("android.view.SurfaceControl");
                surfaceControl
                        .getMethod("destroyDisplay", IBinder.class)
                        .invoke(null, surfaceFlingerToken);
            } catch (Throwable ignored) {
            }
        }
    }

    /**
     * Test hooks, read from the environment the host starts the agent with:
     * `MIRRYL_MIRROR_DOORS` is a comma list of `helper`, `manager`, `flinger`
     * in the order to try, and `MIRRYL_MIRROR_FLAGS` a decimal flag value for
     * the display manager door. Neither is set in normal use; they exist so a
     * phone can be probed door by door without rebuilding the agent.
     */
    private static Door[] doorsFromEnvironment(Door[] fallback) {
        String value = System.getenv("MIRRYL_MIRROR_DOORS");
        if (value == null || value.trim().isEmpty()) {
            return fallback;
        }
        List<Door> doors = new ArrayList<>();
        for (String name : value.split(",")) {
            switch (name.trim().toLowerCase()) {
                case "helper":
                    doors.add(Door.SYSTEM_HELPER);
                    break;
                case "manager":
                    doors.add(Door.DISPLAY_MANAGER);
                    break;
                case "flinger":
                    doors.add(Door.SURFACE_FLINGER);
                    break;
                default:
                    break;
            }
        }
        return doors.isEmpty() ? fallback : doors.toArray(new Door[0]);
    }

    private static int flagsFromEnvironment() {
        String value = System.getenv("MIRRYL_MIRROR_FLAGS");
        if (value == null || value.trim().isEmpty()) {
            return VIRTUAL_DISPLAY_FLAGS;
        }
        try {
            return Integer.parseInt(value.trim());
        } catch (NumberFormatException ignored) {
            return VIRTUAL_DISPLAY_FLAGS;
        }
    }

    // ---------- Door 0: the system's own mirroring helper ----------

    /**
     * The static helper the platform added in API 34 for shell-side mirroring.
     * It takes the display to mirror and the surface, works the package name
     * out for itself, and returns a VirtualDisplay or null. Absent before 34,
     * where the lookup fails fast and the next door is tried.
     */
    private static MirrorDisplay openSystemHelperDisplay(Surface surface, int width, int height)
            throws Exception {
        Class<?> managerType = Class.forName("android.hardware.display.DisplayManager");
        Method create = managerType.getMethod(
                "createVirtualDisplay",
                String.class,
                int.class,
                int.class,
                int.class,
                Surface.class);
        Object display = invokeUnwrapped(
                create, null, DISPLAY_NAME, width, height, VideoStreamMain.DISPLAY_ID, surface);
        if (display == null) {
            throw new IllegalStateException("the system mirroring helper returned no display");
        }
        return new MirrorDisplay("system mirroring helper", display, null);
    }

    // ---------- Door 1: the display manager ----------

    private static MirrorDisplay openVirtualDisplay(
            Context context,
            Surface surface,
            int width,
            int height,
            int flags)
            throws Exception {
        // VirtualDisplayConfig exists from API 30. Older releases take the
        // same parameters spread out as arguments, but no release older than
        // 34 lets the shell mirror this way, so there is nothing to gain from
        // supporting that shape too: Class.forName fails and the other door is
        // tried.
        Class<?> configType =
                Class.forName("android.hardware.display.VirtualDisplayConfig");
        Class<?> builderType =
                Class.forName("android.hardware.display.VirtualDisplayConfig$Builder");
        Object builder = builderType
                .getConstructor(String.class, int.class, int.class, int.class)
                .newInstance(DISPLAY_NAME, width, height, 420);
        builderType.getMethod("setFlags", int.class).invoke(builder, flags);
        builderType.getMethod("setSurface", Surface.class).invoke(builder, surface);
        builderType
                .getMethod("setDisplayIdToMirror", int.class)
                .invoke(builder, VideoStreamMain.DISPLAY_ID);
        Object config = builderType.getMethod("build").invoke(builder);

        Class<?> globalType =
                Class.forName("android.hardware.display.DisplayManagerGlobal");
        Object global = globalType.getMethod("getInstance").invoke(null);
        Method create = findCreateVirtualDisplay(globalType, configType);
        Class<?>[] parameters = create.getParameterTypes();
        Object[] arguments = new Object[parameters.length];
        boolean contextGiven = false;
        for (int index = 0; index < parameters.length; index++) {
            if (parameters[index] == configType) {
                arguments[index] = config;
            } else if (parameters[index] == Context.class && !contextGiven) {
                // The first Context is the caller's; a second one (31–33) is
                // the optional window context, and null is what the public
                // DisplayManager.createVirtualDisplay passes for it.
                arguments[index] = context;
                contextGiven = true;
            } else {
                // MediaProjection, VirtualDisplay.Callback, Handler, Executor,
                // window Context: all optional, all null.
                arguments[index] = null;
            }
        }
        Object display = invokeUnwrapped(create, global, arguments);
        if (display == null) {
            throw new IllegalStateException("createVirtualDisplay returned null");
        }
        return new MirrorDisplay(
                "display manager (" + parameters.length + "-argument createVirtualDisplay, flags "
                        + flags + ")",
                display,
                null);
    }

    /**
     * The createVirtualDisplay overload that takes a VirtualDisplayConfig,
     * whatever else it takes on this release.
     */
    private static Method findCreateVirtualDisplay(Class<?> globalType, Class<?> configType)
            throws NoSuchMethodException {
        for (Method method : globalType.getMethods()) {
            if (!"createVirtualDisplay".equals(method.getName())) {
                continue;
            }
            for (Class<?> parameter : method.getParameterTypes()) {
                if (parameter == configType) {
                    return method;
                }
            }
        }
        throw new NoSuchMethodException(
                "DisplayManagerGlobal.createVirtualDisplay(VirtualDisplayConfig, …) on API "
                        + Build.VERSION.SDK_INT);
    }

    // ---------- Door 2: SurfaceFlinger ----------

    private static MirrorDisplay openSurfaceFlingerDisplay(
            Surface surface,
            int width,
            int height,
            int[] sourceSize,
            int layerStack)
            throws Exception {
        Class<?> surfaceControl = Class.forName("android.view.SurfaceControl");
        Method createDisplay =
                surfaceControl.getMethod("createDisplay", String.class, boolean.class);
        // Never secure: the shell holds CAPTURE_SECURE_VIDEO_OUTPUT on no
        // release, so a secure display would be refused everywhere. Protected
        // content (DRM video, apps that set FLAG_SECURE) mirrors as black,
        // which is what every mirroring tool shows for it.
        IBinder token = (IBinder) invokeUnwrapped(createDisplay, null, DISPLAY_NAME, false);
        if (token == null) {
            throw new IllegalStateException("SurfaceControl.createDisplay returned null");
        }
        try {
            int sourceWidth = sourceSize != null ? sourceSize[0] : width;
            int sourceHeight = sourceSize != null ? sourceSize[1] : height;
            Class<?> transactionType = Class.forName("android.view.SurfaceControl$Transaction");
            Object transaction = transactionType.getConstructor().newInstance();
            transactionType
                    .getMethod("setDisplaySurface", IBinder.class, Surface.class)
                    .invoke(transaction, token, surface);
            // Orientation 0: the layer stack is already in the display's
            // current rotation, and the capture size was turned to match it
            // by the caller, so the whole source maps onto the whole surface.
            transactionType
                    .getMethod(
                            "setDisplayProjection",
                            IBinder.class,
                            int.class,
                            Rect.class,
                            Rect.class)
                    .invoke(
                            transaction,
                            token,
                            0,
                            new Rect(0, 0, sourceWidth, sourceHeight),
                            new Rect(0, 0, width, height));
            transactionType
                    .getMethod("setDisplayLayerStack", IBinder.class, int.class)
                    .invoke(transaction, token, layerStack);
            transactionType.getMethod("apply").invoke(transaction);
            try {
                transactionType.getMethod("close").invoke(transaction);
            } catch (Throwable ignored) {
                // Closeable only from API 29; nothing to free before that.
            }
        } catch (Throwable error) {
            try {
                surfaceControl.getMethod("destroyDisplay", IBinder.class).invoke(null, token);
            } catch (Throwable ignored) {
            }
            throw error;
        }
        return new MirrorDisplay(
                "surfaceflinger (layer stack " + layerStack + ")",
                null,
                token);
    }

    // ---------- Helpers ----------

    /** Invokes and rethrows the real cause, so a SecurityException reads as one. */
    private static Object invokeUnwrapped(Method method, Object target, Object... arguments)
            throws Exception {
        try {
            return method.invoke(target, arguments);
        } catch (InvocationTargetException wrapped) {
            Throwable cause = wrapped.getCause();
            if (cause instanceof Exception) {
                throw (Exception) cause;
            }
            if (cause instanceof Error) {
                throw (Error) cause;
            }
            throw wrapped;
        }
    }

    /** One line for a throwable: its type and the first line of its message. */
    static String summarize(Throwable error) {
        String message = error.getMessage();
        String name = error.getClass().getSimpleName();
        if (message == null || message.isEmpty()) {
            return name;
        }
        // Framework messages run to several lines; the first one says it all.
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
}
