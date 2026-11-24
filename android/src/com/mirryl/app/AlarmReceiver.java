package com.mirryl.app;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * Fired by the periodic safety-net alarm scheduled in WifiAdbHelper.scheduleNextCheck.
 * Re-enables wireless debugging (no-op if already on) and reschedules the next check.
 */
public class AlarmReceiver extends BroadcastReceiver {

    @Override
    public void onReceive(Context context, Intent intent) {
        Context appContext = context.getApplicationContext();
        WifiAdbHelper.autoEnable(appContext);
        WifiAdbHelper.scheduleNextCheck(appContext);
        // Re-arm the network-triggered job too: if it was ever lost (force-stop,
        // "clear data"), this alarm is the only thing left that can bring it
        // back, and without it recovery falls back to this 10-minute cycle.
        WifiAdbHelper.scheduleNetworkCheck(appContext);
        WifiAdbHelper.scheduleSwitchWatch(appContext);
    }
}
