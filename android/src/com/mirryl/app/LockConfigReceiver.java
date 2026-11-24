package com.mirryl.app;

import android.content.BroadcastReceiver;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.provider.Settings;

import java.util.Set;

/**
 * How Mirryl on the PC sets the PIN protection, over adb:
 * {@code am broadcast -n com.mirryl.app/.LockConfigReceiver --es op <op> ...}.
 *
 * Only a sender holding WRITE_SECURE_SETTINGS gets through (the manifest asks
 * for it): adb's shell has it, ordinary apps cannot. The answer goes back as the
 * broadcast's result data, which am prints.
 *
 * Ops: get; lock / unlock (--es package P); set_pin (--es pin N).
 */
public class LockConfigReceiver extends BroadcastReceiver {
    static final int VERSION = 1;

    @Override
    public void onReceive(Context context, Intent intent) {
        String op = intent.getStringExtra("op");
        String pkg = intent.getStringExtra("package");
        String result;
        if ("get".equals(op)) {
            result = state(context);
        } else if ("set_pin".equals(op)) {
            String pin = intent.getStringExtra("pin");
            if (pin == null || !pin.matches("[0-9]{4,12}")) {
                result = "error: the PIN must be 4 to 12 digits";
            } else {
                AppLock.setPin(context, pin);
                result = "ok";
            }
        } else if (("lock".equals(op) || "unlock".equals(op)) && pkg != null && pkg.matches("[A-Za-z0-9_.]+")) {
            if ("lock".equals(op) && !AppLock.hasPin(context)) {
                result = "error: no PIN yet";
            } else {
                AppLock.setLocked(context, pkg, "lock".equals(op));
                result = "ok";
            }
        } else {
            result = "error: unknown request";
        }
        setResultData(result);
    }

    private static String state(Context context) {
        StringBuilder json = new StringBuilder();
        json.append("{\"version\":").append(VERSION);
        json.append(",\"pin\":").append(AppLock.hasPin(context));
        json.append(",\"service\":").append(serviceOn(context));
        json.append(",\"locked\":[");
        Set<String> locked = AppLock.locked(context);
        boolean first = true;
        for (String pkg : locked) {
            if (!first) {
                json.append(',');
            }
            first = false;
            json.append('"').append(pkg).append('"');
        }
        return json.append("]}").toString();
    }

    private static boolean serviceOn(Context context) {
        String enabled = Settings.Secure.getString(context.getContentResolver(),
                Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES);
        if (enabled == null) {
            return false;
        }
        String mine = new ComponentName(context, AppLockService.class).flattenToString();
        String shortName = new ComponentName(context, AppLockService.class).flattenToShortString();
        for (String part : enabled.split(":")) {
            if (part.equalsIgnoreCase(mine) || part.equalsIgnoreCase(shortName)) {
                return true;
            }
        }
        return false;
    }
}
