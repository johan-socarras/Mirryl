package dev.mirador.agent;

import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.Arrays;
import java.util.Set;
import java.util.TreeSet;

public final class ApiProbeMain {
    private ApiProbeMain() {
    }

    public static void main(String[] arguments) {
        inspect(
                "android.view.SurfaceControl",
                new String[] {
                    "createDisplay",
                    "destroyDisplay",
                    "getPhysicalDisplayIds",
                    "getPhysicalDisplayToken",
                    "getInternalDisplayToken"
                });
        inspect(
                "android.view.SurfaceControl$Transaction",
                new String[] {
                    "setDisplaySurface",
                    "setDisplayProjection",
                    "setDisplayLayerStack",
                    "apply"
                });
        inspect(
                "com.android.server.display.DisplayControl",
                new String[] {
                    "createVirtualDisplay",
                    "destroyVirtualDisplay",
                    "getPhysicalDisplayIds",
                    "getPhysicalDisplayToken"
                });
        inspect(
                "android.window.ScreenCapture",
                new String[] {
                    "captureDisplay",
                    "captureLayers"
                });
        inspect(
                "android.hardware.display.DisplayManagerGlobal",
                new String[] {
                    "getInstance",
                    "createVirtualDisplay",
                    "setVirtualDisplaySurface",
                    "releaseVirtualDisplay"
                });
        inspect(
                "android.hardware.display.VirtualDisplayConfig$Builder",
                new String[] {
                    "setFlags",
                    "setSurface",
                    "setDisplayIdToMirror",
                    "build"
                });
        inspect(
                "android.app.ActivityThread",
                new String[] {
                    "systemMain",
                    "getSystemContext",
                    "currentActivityThread"
                });
    }

    private static void inspect(String className, String[] interestingNames) {
        System.out.println("CLASS " + className);
        try {
            Class<?> type = Class.forName(className);
            System.out.println("  loaded=true");
            for (Constructor<?> constructor : type.getDeclaredConstructors()) {
                System.out.println(
                        "  ctor "
                                + Modifier.toString(constructor.getModifiers())
                                + " "
                                + Arrays.toString(constructor.getParameterTypes()));
            }

            Set<String> interesting = new TreeSet<>(Arrays.asList(interestingNames));
            for (Method method : type.getDeclaredMethods()) {
                if (interesting.contains(method.getName())) {
                    System.out.println(
                            "  method "
                                    + Modifier.toString(method.getModifiers())
                                    + " "
                                    + method.getName()
                                    + Arrays.toString(method.getParameterTypes())
                                    + " -> "
                                    + method.getReturnType().getName());
                }
            }
        } catch (Throwable error) {
            System.out.println(
                    "  loaded=false "
                            + error.getClass().getName()
                            + ": "
                            + error.getMessage());
        }
    }
}
