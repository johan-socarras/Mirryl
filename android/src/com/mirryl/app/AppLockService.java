package com.mirryl.app;

import android.accessibilityservice.AccessibilityService;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.view.accessibility.AccessibilityEvent;
import android.view.inputmethod.InputMethodInfo;
import android.view.inputmethod.InputMethodManager;

import java.util.HashSet;
import java.util.Set;

/**
 * Watches which app comes to the front, and puts the PIN screen over the ones
 * protected with a PIN (AppLock). It reads only the package name of the window
 * that opened: no content of any screen, and nothing leaves the phone.
 *
 * Turned on by Mirryl on the PC over adb when the first app is protected, so
 * nobody has to find it in Accessibility settings.
 */
public class AppLockService extends AccessibilityService {
    private final Set<String> ignored = new HashSet<String>();
    private final BroadcastReceiver screenOff = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent) {
            AppLock.relock();
        }
    };

    @Override
    protected void onServiceConnected() {
        super.onServiceConnected();
        ignored.clear();
        ignored.add(getPackageName());
        ignored.add("com.android.systemui");
        // A keyboard opening inside an unlocked app is not leaving it.
        InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        if (imm != null) {
            for (InputMethodInfo info : imm.getInputMethodList()) {
                ignored.add(info.getPackageName());
            }
        }
        registerReceiver(screenOff, new IntentFilter(Intent.ACTION_SCREEN_OFF));
    }

    @Override
    public void onAccessibilityEvent(AccessibilityEvent event) {
        if (event == null || event.getEventType() != AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED) {
            return;
        }
        CharSequence name = event.getPackageName();
        if (name == null) {
            return;
        }
        String pkg = AppLock.ownerOf(name.toString());
        if (ignored.contains(pkg)) {
            return;
        }
        if (!pkg.equals(AppLock.unlockedPackage())) {
            AppLock.relock();
        }
        if (AppLock.mustAsk(this, pkg)) {
            Intent lock = new Intent(this, LockActivity.class);
            lock.putExtra(LockActivity.EXTRA_PACKAGE, pkg);
            lock.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK
                    | Intent.FLAG_ACTIVITY_CLEAR_TOP
                    | Intent.FLAG_ACTIVITY_EXCLUDE_FROM_RECENTS);
            startActivity(lock);
        }
    }

    @Override
    public void onInterrupt() {
    }

    @Override
    public void onDestroy() {
        try {
            unregisterReceiver(screenOff);
        } catch (IllegalArgumentException ignoredError) {
            // Never registered: the service died before it connected.
        }
        super.onDestroy();
    }
}
