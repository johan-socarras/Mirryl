package dev.mirador.agent;

import android.app.AppOpsManager;
import android.content.Context;
import android.os.Build;
import android.os.Process;

import java.lang.reflect.Method;

/**
 * Prints what the app-ops service answers for the shell's RECORD_AUDIO op, the
 * way the audio server asks it. Diagnostic only: run it by hand with
 * {@code app_process / dev.mirador.agent.AppOpProbeMain} when an audio
 * capture is refused with "denied by app op".
 */
public final class AppOpProbeMain {
    private AppOpProbeMain() {
    }

    public static void main(String[] arguments) {
        try {
            if (android.os.Looper.myLooper() == null) {
                android.os.Looper.prepare();
            }
            Class<?> activityThread = Class.forName("android.app.ActivityThread");
            Object thread = activityThread.getDeclaredMethod("systemMain").invoke(null);
            Context context =
                    (Context) activityThread.getDeclaredMethod("getSystemContext").invoke(thread);
            AppOpsManager ops = (AppOpsManager) context.getSystemService(Context.APP_OPS_SERVICE);
            System.out.println("android=" + Build.VERSION.RELEASE + " api=" + Build.VERSION.SDK_INT
                    + " uid=" + Process.myUid());
            String[] packages = {"com.android.shell", "uid:2000", ""};
            for (String pkg : packages) {
                try {
                    int check = ops.checkOpNoThrow(AppOpsManager.OPSTR_RECORD_AUDIO, Process.myUid(), pkg);
                    System.out.println("checkOpNoThrow(RECORD_AUDIO, " + Process.myUid() + ", \"" + pkg + "\") = " + check);
                } catch (Throwable error) {
                    System.out.println("checkOpNoThrow(\"" + pkg + "\") threw " + error);
                }
                try {
                    Method raw = AppOpsManager.class.getMethod(
                            "unsafeCheckOpRawNoThrow", String.class, int.class, String.class);
                    System.out.println("unsafeCheckOpRawNoThrow(\"" + pkg + "\") = "
                            + raw.invoke(ops, AppOpsManager.OPSTR_RECORD_AUDIO, Process.myUid(), pkg));
                } catch (Throwable error) {
                    System.out.println("unsafeCheckOpRawNoThrow(\"" + pkg + "\") threw " + error);
                }
            }
            try {
                int note = ops.noteOpNoThrow(AppOpsManager.OPSTR_RECORD_AUDIO, Process.myUid(), "com.android.shell");
                System.out.println("noteOpNoThrow(RECORD_AUDIO, com.android.shell) = " + note);
            } catch (Throwable error) {
                System.out.println("noteOpNoThrow threw " + error);
            }
            // With an argument, keep asking for that many half-seconds: run
            // "am start -n com.android.shell/.HeapDumpActivity" meanwhile to
            // see whether the answer follows the uid's foreground state.
            int polls = arguments.length > 0 ? Integer.parseInt(arguments[0]) : 0;
            for (int poll = 0; poll < polls; poll++) {
                Thread.sleep(500);
                System.out.println("t+" + ((poll + 1) * 500) + "ms checkOp="
                        + ops.checkOpNoThrow(AppOpsManager.OPSTR_RECORD_AUDIO, Process.myUid(), "com.android.shell"));
            }
            System.out.println("MODE_ALLOWED=" + AppOpsManager.MODE_ALLOWED
                    + " MODE_IGNORED=" + AppOpsManager.MODE_IGNORED
                    + " MODE_ERRORED=" + AppOpsManager.MODE_ERRORED
                    + " MODE_FOREGROUND=4");
        } catch (Throwable error) {
            error.printStackTrace(System.out);
        }
    }
}
