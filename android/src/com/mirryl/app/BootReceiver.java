package com.mirryl.app;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.os.Handler;
import android.os.Looper;

/**
 * Re-enables Android's Wireless Debugging on every boot, since Android turns it
 * off on shutdown/reboot by design. Retries a few times because on boot the
 * settings provider / wifi stack may not be ready yet on the first attempt.
 *
 * Also runs right after this app is updated (MY_PACKAGE_REPLACED). An update
 * keeps the persisted network job but not the switch watch, which cannot be
 * persisted: after an update only the network job is left, so the fast path
 * stays unarmed until the app is opened or the 10-minute alarm fires. Mirryl on
 * the PC opens the app once after installing an update, but this receiver does
 * not rely on that.
 */
public class BootReceiver extends BroadcastReceiver {

    private static final int[] RETRY_DELAYS_MS = {2000, 5000, 10000, 20000, 30000};

    @Override
    public void onReceive(Context context, Intent intent) {
        final Context appContext = context.getApplicationContext();
        if (Intent.ACTION_MY_PACKAGE_REPLACED.equals(intent.getAction())) {
            // Settings and the network are already up after an update: one
            // attempt is enough, no retries.
            WifiAdbHelper.autoEnable(appContext);
            WifiAdbHelper.scheduleNextCheck(appContext);
            WifiAdbHelper.scheduleNetworkCheck(appContext);
            WifiAdbHelper.scheduleSwitchWatch(appContext);
            return;
        }
        if (!Intent.ACTION_BOOT_COMPLETED.equals(intent.getAction())) {
            return;
        }
        Handler handler = new Handler(Looper.getMainLooper());
        for (int delay : RETRY_DELAYS_MS) {
            handler.postDelayed(new Runnable() {
                @Override
                public void run() {
                    WifiAdbHelper.autoEnable(appContext);
                }
            }, delay);
        }
        WifiAdbHelper.scheduleNextCheck(appContext);
        WifiAdbHelper.scheduleNetworkCheck(appContext);
        // The switch watch is not persisted (content-triggered jobs cannot
        // be), so this is the one place that re-arms it for a phone that is
        // never opened again.
        WifiAdbHelper.scheduleSwitchWatch(appContext);
    }
}
