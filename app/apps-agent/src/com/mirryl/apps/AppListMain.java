package com.mirryl.apps;

import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.Drawable;
import android.os.Build;
import android.os.Looper;
import android.util.Base64;

import java.io.ByteArrayOutputStream;
import java.io.PrintStream;
import java.util.Arrays;
import java.util.HashMap;
import java.util.HashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * Lists the apps of the phone for the Apps screen of the PC app: name, icon and
 * state of each one, one JSON object per line on stdout.
 *
 * Run by the PC over adb as the shell user, never installed:
 * {@code CLASSPATH=/data/local/tmp/mirryl-apps.jar app_process / com.mirryl.apps.AppListMain}.
 * The shell user sees every package, so the phone app needs no permission to
 * list them, and the list goes only to the PC that asked for it.
 *
 * Line 1: {"home":"<default launcher package>","sdk":<api level>}.
 * Then, per installed app, a line "BEGIN <package>" and its JSON line. Ends
 * with the line "END", so the PC can tell a complete list from one cut short.
 *
 * Some phones abort the whole process (natively, past any catch) while one
 * particular app's icon is drawn - a Samsung font assertion was seen. The
 * BEGIN line names that app, and the PC runs this again with it in
 * --noicon=a,b (no icon) or --bare=c,d (not even its name read).
 */
public final class AppListMain {
    private static final int ICON_SIZE = 72;
    /** ApplicationInfo.FLAG_SUSPENDED, hidden from the public API. */
    private static final int FLAG_SUSPENDED = 1 << 30;
    private static final int MATCH_DISABLED_COMPONENTS = 0x00000200;
    private static final int MATCH_DISABLED_UNTIL_USED_COMPONENTS = 0x00008000;

    private AppListMain() {
    }

    public static void main(String[] arguments) {
        PrintStream out = new PrintStream(System.out, false);
        Set<String> noIcon = new HashSet<String>();
        Set<String> bare = new HashSet<String>();
        for (String argument : arguments) {
            if (argument.startsWith("--noicon=")) {
                noIcon.addAll(Arrays.asList(argument.substring(9).split(",")));
            } else if (argument.startsWith("--bare=")) {
                bare.addAll(Arrays.asList(argument.substring(7).split(",")));
            }
        }
        try {
            if (Looper.myLooper() == null) {
                Looper.prepare();
            }
            Context context = systemContext();
            PackageManager pm = context.getPackageManager();

            Map<String, String> launchers = launcherActivities(pm);
            String home = homePackage(pm);

            out.println("{\"home\":" + quote(home) + ",\"sdk\":" + Build.VERSION.SDK_INT + "}");
            List<PackageInfo> packages = pm.getInstalledPackages(
                    MATCH_DISABLED_COMPONENTS | MATCH_DISABLED_UNTIL_USED_COMPONENTS);
            for (PackageInfo info : packages) {
                ApplicationInfo app = info.applicationInfo;
                if (app == null) {
                    continue;
                }
                out.println("BEGIN " + info.packageName);
                out.flush();
                try {
                    if (bare.contains(info.packageName)) {
                        throw new IllegalStateException("skipped");
                    }
                    out.println(describe(pm, info, app, launchers.get(info.packageName),
                            !noIcon.contains(info.packageName)));
                } catch (Throwable error) {
                    // One app that cannot be read does not cost the whole list.
                    out.println("{\"package\":" + quote(info.packageName)
                            + ",\"label\":" + quote(info.packageName)
                            + ",\"system\":" + ((app.flags & ApplicationInfo.FLAG_SYSTEM) != 0)
                            + ",\"enabled\":" + app.enabled
                            + ",\"suspended\":" + ((app.flags & FLAG_SUSPENDED) != 0)
                            + ",\"activity\":" + quote(launcherOf(launchers, info.packageName))
                            + ",\"version\":\"\""
                            + ",\"apks\":[],\"icon\":\"\"}");
                }
                out.flush();
            }
            out.println("END");
            out.flush();
        } catch (Throwable error) {
            out.println("ERROR " + error);
            out.flush();
            System.exit(1);
        }
        System.exit(0);
    }

    private static String launcherOf(Map<String, String> launchers, String pkg) {
        String activity = launchers.get(pkg);
        return activity == null ? "" : activity;
    }

    private static String describe(PackageManager pm, PackageInfo info, ApplicationInfo app, String activity,
            boolean withIcon) {
        boolean system = (app.flags & ApplicationInfo.FLAG_SYSTEM) != 0;
        boolean suspended = (app.flags & FLAG_SUSPENDED) != 0;
        CharSequence label = app.loadLabel(pm);
        StringBuilder apks = new StringBuilder("[");
        if (app.sourceDir != null) {
            apks.append(quote(app.sourceDir));
        }
        if (app.splitSourceDirs != null) {
            for (String split : app.splitSourceDirs) {
                if (apks.length() > 1) {
                    apks.append(',');
                }
                apks.append(quote(split));
            }
        }
        apks.append(']');
        // Only apps with a place in the app drawer get a picture: the others
        // would all show Android's same default robot, at a cost per app.
        String icon = activity != null && withIcon ? iconPng(app.loadIcon(pm)) : "";
        return "{\"package\":" + quote(info.packageName)
                + ",\"label\":" + quote(label == null ? info.packageName : label.toString())
                + ",\"system\":" + system
                + ",\"enabled\":" + app.enabled
                + ",\"suspended\":" + suspended
                + ",\"activity\":" + quote(activity == null ? "" : activity)
                + ",\"version\":" + quote(info.versionName == null ? "" : info.versionName)
                + ",\"installed\":" + info.firstInstallTime
                + ",\"updated\":" + info.lastUpdateTime
                + ",\"apks\":" + apks
                + ",\"icon\":" + quote(icon)
                + "}";
    }

    /** Package -> "package/activity" of its entry in the app drawer, disabled ones included. */
    private static Map<String, String> launcherActivities(PackageManager pm) {
        Map<String, String> result = new HashMap<String, String>();
        Intent intent = new Intent(Intent.ACTION_MAIN);
        intent.addCategory(Intent.CATEGORY_LAUNCHER);
        List<ResolveInfo> found = pm.queryIntentActivities(
                intent, MATCH_DISABLED_COMPONENTS | MATCH_DISABLED_UNTIL_USED_COMPONENTS);
        for (ResolveInfo resolve : found) {
            if (resolve.activityInfo == null) {
                continue;
            }
            String pkg = resolve.activityInfo.packageName;
            if (!result.containsKey(pkg)) {
                result.put(pkg, pkg + "/" + resolve.activityInfo.name);
            }
        }
        return result;
    }

    private static String homePackage(PackageManager pm) {
        try {
            Intent intent = new Intent(Intent.ACTION_MAIN);
            intent.addCategory(Intent.CATEGORY_HOME);
            ResolveInfo resolve = pm.resolveActivity(intent, PackageManager.MATCH_DEFAULT_ONLY);
            if (resolve != null && resolve.activityInfo != null) {
                return resolve.activityInfo.packageName;
            }
        } catch (Throwable ignored) {
            // No answer is "no launcher known", not a failed list.
        }
        return "";
    }

    private static String iconPng(Drawable drawable) {
        if (drawable == null) {
            return "";
        }
        Bitmap bitmap = Bitmap.createBitmap(ICON_SIZE, ICON_SIZE, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        drawable.setBounds(0, 0, ICON_SIZE, ICON_SIZE);
        drawable.draw(canvas);
        ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        bitmap.compress(Bitmap.CompressFormat.PNG, 100, bytes);
        bitmap.recycle();
        return Base64.encodeToString(bytes.toByteArray(), Base64.NO_WRAP);
    }

    /** The system context, the way the stream agent gets it: this process is not an app. */
    private static Context systemContext() throws Exception {
        Class<?> activityThread = Class.forName("android.app.ActivityThread");
        Object thread = activityThread.getDeclaredMethod("systemMain").invoke(null);
        return (Context) activityThread.getDeclaredMethod("getSystemContext").invoke(thread);
    }

    private static String quote(String value) {
        StringBuilder text = new StringBuilder(value.length() + 2);
        text.append('"');
        for (int i = 0; i < value.length(); i++) {
            char c = value.charAt(i);
            switch (c) {
                case '"':
                    text.append("\\\"");
                    break;
                case '\\':
                    text.append("\\\\");
                    break;
                case '\n':
                    text.append("\\n");
                    break;
                case '\r':
                    text.append("\\r");
                    break;
                case '\t':
                    text.append("\\t");
                    break;
                default:
                    if (c < 0x20) {
                        text.append(String.format("\\u%04x", (int) c));
                    } else {
                        text.append(c);
                    }
            }
        }
        return text.append('"').toString();
    }
}
