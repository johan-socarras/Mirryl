package com.mirryl.app;

import android.app.job.JobParameters;
import android.app.job.JobService;

/**
 * Re-enables wireless debugging as soon as the device has a network again.
 *
 * The 10-minute alarm in WifiAdbHelper.scheduleNextCheck is a safety net, not a
 * recovery path: when WiFi is toggled off and back on, Android turns the
 * Wireless debugging switch off, and nothing brought it back until the next
 * alarm - up to 10 minutes of the phone simply being unreachable, which reads
 * to the user as "it never comes back".
 *
 * A manifest-declared receiver cannot help here: CONNECTIVITY_ACTION stopped
 * being deliverable to manifest receivers in Android 7, and this app has no
 * long-lived process to register a NetworkCallback from. A persisted job with a
 * network constraint is the supported way to be woken when connectivity
 * returns, and it needs no foreground service or permanent notification.
 *
 * The job is one-shot and re-arms itself on every run, so there is always one
 * pending job waiting for the next time the network comes back.
 */
public class NetworkJobService extends JobService {

    @Override
    public boolean onStartJob(JobParameters params) {
        // Only write when the switch is actually off. Writing 1 over 1 is a
        // no-op on the SettingsProvider (verified on a real device: the debug
        // port and the live adb connection both survive it), but checking first
        // keeps this from depending on that behaviour holding on every OEM.
        // autoEnable also runs with the switch on: that is how it learns a
        // write of ours stuck, or that the user allowed the network.
        // The switch watch checks for a refusal before its brake: Android
        // turning our write back off is exactly the change that wakes it, and
        // the brake would otherwise hide it.
        boolean fromSwitchWatch = params.getJobId() == WifiAdbHelper.SWITCH_JOB_ID;
        if (WifiAdbHelper.isEnabled(this) || !fromSwitchWatch) {
            WifiAdbHelper.autoEnable(getApplicationContext());
        } else if (!WifiAdbHelper.refusedHere(this, WifiAdbHelper.activeNetworkHandle(this))
                && WifiAdbHelper.claimSwitchWrite(this)) {
            WifiAdbHelper.autoEnable(getApplicationContext());
        }
        // Two jobs land here and each re-arms only itself: the switch watch
        // (WifiAdbHelper.scheduleSwitchWatch) because a content-triggered job
        // ends once it has run, the network job to keep its one-minute
        // backstop going.
        if (fromSwitchWatch) {
            WifiAdbHelper.scheduleSwitchWatch(getApplicationContext());
        } else {
            WifiAdbHelper.scheduleNetworkCheck(getApplicationContext());
            // Belt and braces: if the switch watch got lost (an update, a
            // force-stop), the one-minute job puts it back. Only when it is
            // missing - replacing a pending one would drop a change it has
            // already seen and is waiting on the network to act on.
            WifiAdbHelper.ensureSwitchWatch(getApplicationContext());
        }
        // Everything above is a settings read plus at most one write, so the
        // work is already done by the time this returns.
        return false;
    }

    @Override
    public boolean onStopJob(JobParameters params) {
        // Ask for a reschedule if the system stopped us mid-flight; losing this
        // job would leave the phone with only the 10-minute alarm again.
        return true;
    }
}
