package com.mirryl.app;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.DialogInterface;
import android.content.Intent;
import android.content.res.ColorStateList;
import android.graphics.Typeface;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

public class MainActivity extends Activity {

    /**
     * How often the open screen re-reads the real state. Wireless debugging and
     * the WiFi address change behind the app's back - Android turns the switch
     * off when WiFi drops, and the background job turns it back on - so a screen
     * that only refreshed on open showed a stale answer for as long as it was
     * left open, and the only way to see the truth was to press Refresh.
     */
    private static final long REFRESH_INTERVAL_MS = 2000;

    private View permissionDot;
    private TextView permissionText;
    private TextView permissionHint;
    private View statusDot;
    private TextView statusText;
    private TextView secondaryText;
    private TextView batteryBody;
    private Button batteryButton;
    // Guards against refreshTick (every 2s) stacking a second AlertDialog on
    // top of one the owner has not answered yet.
    private boolean pcReplacementDialogShowing = false;
    private final Handler refreshHandler = new Handler(Looper.getMainLooper());
    private final Runnable refreshTick = new Runnable() {
        @Override
        public void run() {
            refresh();
            refreshHandler.postDelayed(this, REFRESH_INTERVAL_MS);
        }
    };

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        setTheme(Theme.platformTheme(this));
        super.onCreate(savedInstanceState);
        setContentView(buildLayout());
        Theme.applySystemBars(this);
        // Self-heal: the alarm chain and the network job are both one-shot and
        // re-arm themselves, so a force-stop or "clear data" breaks the chain
        // until the next reboot. Opening the app is the one moment we can be
        // sure to fix that; rescheduling an existing alarm or job replaces it.
        WifiAdbHelper.scheduleNextCheck(this);
        WifiAdbHelper.scheduleNetworkCheck(this);
        WifiAdbHelper.scheduleSwitchWatch(this);
        // Opening the app is an explicit "I want this working now", so do not
        // make the user also press the button: turn it on if it is off. The
        // 10-minute alarm already re-enables it unattended, so this only makes
        // the app agree with what it does in the background anyway.
        if (!WifiAdbHelper.isEnabled(this)) {
            WifiAdbHelper.enable(this);
        }
        refresh();
    }

    @Override
    protected void onResume() {
        super.onResume();
        // Ticks only while the screen is actually in front of the user; onPause
        // stops it so nothing polls in the background.
        refreshHandler.removeCallbacks(refreshTick);
        refreshHandler.post(refreshTick);
    }

    @Override
    protected void onPause() {
        super.onPause();
        refreshHandler.removeCallbacks(refreshTick);
    }

    private LinearLayout buildLayout() {
        int pad = dp(20);
        int topBottomPad = dp(14);

        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        Theme.applyContentInsets(root, pad, topBottomPad, pad, topBottomPad);
        root.setBackground(Theme.screenBackground(this));

        root.addView(buildHeaderRow());

        // The cards stay at the top and scroll if they ever outgrow the screen;
        // the actions are anchored at the bottom, within thumb reach.
        LinearLayout cards = new LinearLayout(this);
        cards.setOrientation(LinearLayout.VERTICAL);
        cards.addView(buildStatusCard());
        cards.addView(buildFinishSetupCard());
        cards.addView(buildBatteryOptimizationCard());

        ScrollView scroll = new ScrollView(this);
        scroll.setVerticalScrollBarEnabled(false);
        scroll.addView(cards);
        scroll.setLayoutParams(new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f));
        root.addView(scroll);

        Button enableButton = Theme.primaryButton(this, "Enable wireless debugging now");
        enableButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                // enable() fails silently by design (a caller cannot tell "no PC
                // has ever connected" from "the OS refused the write" - see its
                // javadoc), which read as a dead button in testing, when the
                // missing piece was the seal from a computer connection.
                // Checking both unlock paths ourselves first lets
                // the dialog explain the actual reason instead of just failing
                // quiet - matches the same OR enable() itself uses.
                if (!WifiAdbHelper.hasFreshSeal(MainActivity.this)
                        && !WifiAdbHelper.isMatriculated(MainActivity.this)) {
                    showRequiresPcDialog();
                    return;
                }
                boolean enabled = WifiAdbHelper.enable(MainActivity.this);
                refresh();
                if (!enabled) {
                    showEnableFailedDialog();
                }
            }
        });
        // Without this margin the anchored buttons sat flush against whatever
        // card happened to be at the bottom of the scroll content - fine with
        // two cards, but the third (battery) card made it visibly cramped,
        // sometimes reading as the two button rows touching each other.
        LinearLayout.LayoutParams enableParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        enableParams.topMargin = dp(8);
        enableButton.setLayoutParams(enableParams);
        root.addView(enableButton);

        // No setup guide here: Mirryl on the PC installs this app and walks
        // through the whole setup itself, phone steps included.
        root.addView(buildLegalFooter());

        return root;
    }

    /**
     * The app's version under the buttons, small and muted, so it does not
     * compete with them. Nothing in this app links anywhere: it was installed
     * by Mirryl on the PC, and everything else (the license, the privacy
     * notice) lives there.
     */
    private LinearLayout buildLegalFooter() {
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setGravity(Gravity.CENTER);
        LinearLayout.LayoutParams rowParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        rowParams.topMargin = dp(4);
        row.setLayoutParams(rowParams);

        String version = versionName();
        if (version != null) {
            row.addView(footerText("Mirryl v" + version));
        }
        return row;
    }

    private TextView footerText(String text) {
        TextView view = new TextView(this);
        view.setText(text);
        view.setTextSize(12);
        view.setTextColor(Theme.muted(this));
        return view;
    }

    private String versionName() {
        try {
            return getPackageManager().getPackageInfo(getPackageName(), 0).versionName;
        } catch (android.content.pm.PackageManager.NameNotFoundException e) {
            return null;
        }
    }

    private LinearLayout buildHeaderRow() {
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        row.setGravity(Gravity.CENTER_VERTICAL);
        LinearLayout.LayoutParams rowParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        rowParams.bottomMargin = dp(12);
        row.setLayoutParams(rowParams);

        TextView title = new TextView(this);
        title.setText("Mirryl");
        title.setTextSize(22);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        title.setTextColor(Theme.text(this));
        row.addView(title);

        // The wordmark carries an accent dot, as in the desktop app.
        View mark = new View(this);
        mark.setBackground(Theme.dot(this, Theme.accent(this)));
        LinearLayout.LayoutParams markParams = new LinearLayout.LayoutParams(dp(7), dp(7));
        markParams.leftMargin = dp(9);
        mark.setLayoutParams(markParams);
        row.addView(mark);

        View spacer = new View(this);
        spacer.setLayoutParams(new LinearLayout.LayoutParams(0, 1, 1f));
        row.addView(spacer);

        // Half-circle glyphs, not emoji: Android substitutes its colour emoji
        // font for the sun and moon characters and they land as stickers.
        boolean dark = Theme.isDark(this);
        Button themeButton = Theme.iconButton(this, dark ? "◑" : "◐", 36);
        themeButton.setContentDescription(dark ? "Switch to light theme" : "Switch to dark theme");
        themeButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                Theme.toggle(MainActivity.this);
                recreate();
            }
        });
        row.addView(themeButton);

        return row;
    }

    private LinearLayout buildStatusCard() {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setBackground(Theme.cardBackground(this));
        card.setPadding(dp(14), dp(12), dp(14), dp(12));
        LinearLayout.LayoutParams cardParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        cardParams.bottomMargin = dp(8);
        card.setLayoutParams(cardParams);

        LinearLayout headerRow = new LinearLayout(this);
        headerRow.setOrientation(LinearLayout.HORIZONTAL);
        headerRow.setGravity(Gravity.CENTER_VERTICAL);

        TextView cardTitle = new TextView(this);
        cardTitle.setText("Status");
        Theme.labelStyle(cardTitle);
        cardTitle.setLayoutParams(new LinearLayout.LayoutParams(
                0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f));
        headerRow.addView(cardTitle);

        Button refreshButton = Theme.iconButton(this, "⟳", 34);
        refreshButton.setContentDescription("Refresh");
        refreshButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                refresh();
            }
        });
        headerRow.addView(refreshButton);

        card.addView(headerRow);

        permissionDot = new View(this);
        permissionText = new TextView(this);
        LinearLayout permissionPill = buildPill(permissionDot, permissionText);
        ((LinearLayout.LayoutParams) permissionPill.getLayoutParams()).topMargin = dp(10);
        card.addView(permissionPill);

        statusDot = new View(this);
        statusText = new TextView(this);
        card.addView(buildPill(statusDot, statusText));

        // The "how to fix it" half of the permission line lives outside the
        // pill: inside, it wraps to three lines and the shape falls apart.
        permissionHint = new TextView(this);
        permissionHint.setText("Grant it once from Mirryl on your support PC.");
        permissionHint.setTextSize(12.5f);
        permissionHint.setTextColor(Theme.muted(this));
        permissionHint.setPadding(dp(2), dp(2), 0, 0);
        card.addView(permissionHint);

        View secondaryDivider = new View(this);
        LinearLayout.LayoutParams secondaryDividerParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, dp(1));
        secondaryDividerParams.topMargin = dp(10);
        secondaryDividerParams.bottomMargin = dp(8);
        secondaryDivider.setLayoutParams(secondaryDividerParams);
        secondaryDivider.setBackgroundColor(Theme.border(this));
        card.addView(secondaryDivider);

        secondaryText = new TextView(this);
        secondaryText.setTextSize(12.5f);
        secondaryText.setTypeface(Typeface.MONOSPACE);
        secondaryText.setTextColor(Theme.muted(this));
        card.addView(secondaryText);

        return card;
    }

    /** Outlined pill with a leading status dot. */
    private LinearLayout buildPill(View dot, TextView label) {
        LinearLayout pill = new LinearLayout(this);
        pill.setOrientation(LinearLayout.HORIZONTAL);
        pill.setGravity(Gravity.CENTER_VERTICAL);
        pill.setBackground(Theme.pillBackground(this));
        pill.setPadding(dp(9), dp(5), dp(12), dp(5));
        LinearLayout.LayoutParams pillParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        pillParams.bottomMargin = dp(6);
        pill.setLayoutParams(pillParams);

        LinearLayout.LayoutParams dotParams = new LinearLayout.LayoutParams(dp(7), dp(7));
        dotParams.rightMargin = dp(9);
        dot.setLayoutParams(dotParams);
        pill.addView(dot);

        label.setTextSize(13);
        pill.addView(label);

        return pill;
    }

    private LinearLayout buildFinishSetupCard() {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.HORIZONTAL);
        card.setBackground(Theme.cardBackground(this));
        card.setPadding(dp(14), dp(12), dp(14), dp(12));
        LinearLayout.LayoutParams cardParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        cardParams.bottomMargin = dp(8);
        card.setLayoutParams(cardParams);

        // A plain monitor on a tinted tile, deliberately NOT the Windows logo:
        // Microsoft's brand guidelines say their logos "can never be used without
        // an express license", and name app icons specifically. A generic monitor
        // carries no trademark and echoes the shape in Mirryl's own launcher icon.
        //
        // The asset is a white silhouette with a real alpha channel, tinted at
        // runtime, so one file serves both themes instead of two sets of PNGs.
        ImageView icon = new ImageView(this);
        icon.setImageResource(R.drawable.ic_monitor);
        icon.setImageTintList(ColorStateList.valueOf(Theme.accent(this)));
        icon.setScaleType(ImageView.ScaleType.FIT_CENTER);
        icon.setBackground(Theme.tileBackground(this));
        icon.setPadding(dp(8), dp(8), dp(8), dp(8));
        // Decorative: the title right next to it already says what it means.
        icon.setImportantForAccessibility(View.IMPORTANT_FOR_ACCESSIBILITY_NO);
        LinearLayout.LayoutParams iconParams = new LinearLayout.LayoutParams(dp(38), dp(38));
        iconParams.rightMargin = dp(12);
        icon.setLayoutParams(iconParams);
        card.addView(icon);

        LinearLayout textColumn = new LinearLayout(this);
        textColumn.setOrientation(LinearLayout.VERTICAL);
        textColumn.setLayoutParams(new LinearLayout.LayoutParams(
                0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f));

        TextView cardTitle = new TextView(this);
        cardTitle.setText("Finish setup on your computer");
        cardTitle.setTextSize(15);
        cardTitle.setTypeface(Typeface.DEFAULT_BOLD);
        cardTitle.setTextColor(Theme.text(this));
        textColumn.addView(cardTitle);

        TextView body = new TextView(this);
        body.setText("Open Mirryl on your computer and follow the guided setup there.");
        body.setTextSize(13.5f);
        body.setTextColor(Theme.muted(this));
        body.setPadding(0, dp(4), 0, 0);
        textColumn.addView(body);

        card.addView(textColumn);

        return card;
    }

    /**
     * Optional, secondary card: no icon column on purpose, to read as lighter-
     * weight than "Finish setup on your computer". Body text is updated live
     * in refresh() depending on whether the exemption is in place. Since
     * versionCode 22 the app does not ask for it: Mirryl on the PC sets it over
     * adb when it connects, so this card only says where things stand and
     * links to the system screen where the user can turn it off.
     */
    private LinearLayout buildBatteryOptimizationCard() {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setBackground(Theme.cardBackground(this));
        card.setPadding(dp(14), dp(12), dp(14), dp(12));
        LinearLayout.LayoutParams cardParams = new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT);
        cardParams.bottomMargin = dp(8);
        card.setLayoutParams(cardParams);

        TextView cardTitle = new TextView(this);
        cardTitle.setText("Faster reconnection");
        cardTitle.setTextSize(15);
        cardTitle.setTypeface(Typeface.DEFAULT_BOLD);
        cardTitle.setTextColor(Theme.text(this));
        card.addView(cardTitle);

        batteryBody = new TextView(this);
        batteryBody.setTextSize(13.5f);
        batteryBody.setTextColor(Theme.muted(this));
        batteryBody.setPadding(0, dp(4), 0, dp(8));
        card.addView(batteryBody);

        batteryButton = Theme.ghostButton(this, "Open Battery settings");
        batteryButton.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                WifiAdbHelper.openAppBatterySettings(MainActivity.this);
            }
        });
        card.addView(batteryButton);

        return card;
    }

    /**
     * Shown when the enable button is tapped without any PC having connected
     * recently enough - a case found in testing that deserved an explanation
     * instead of a silent no-op.
     */
    private void showRequiresPcDialog() {
        new AlertDialog.Builder(this)
                .setTitle("Requires Mirryl on a PC")
                .setMessage("This device needs a computer running Mirryl to connect to it "
                        + "at least once before wireless debugging can be turned on from "
                        + "here. Install Mirryl on your PC and connect, then come back and "
                        + "try again.")
                .setPositiveButton("Got it", null)
                .show();
    }

    /**
     * Shown when a PC did connect recently (hasFreshSeal passed) but the OS
     * still refused the write - the WRITE_SECURE_SETTINGS grant itself is
     * missing or was revoked, which the PC's connect step is what restores.
     */
    private void showEnableFailedDialog() {
        new AlertDialog.Builder(this)
                .setTitle("Couldn't turn it on")
                .setMessage("Something is blocking this device from enabling wireless "
                        + "debugging by itself. Reconnect your computer once from Mirryl "
                        + "and try again.")
                .setPositiveButton("Got it", null)
                .show();
    }

    /**
     * Shown when a `.mcert` signed by a PC other than the pinned one shows
     * up - the one door into replacing a lost/broken PC, chosen over a
     * permanent "Forget this PC" button because it only ever appears when
     * it is actually relevant, and over silent auto-replacement because
     * that would let any PC with adb access steal the pairing at will. See
     * WifiAdbHelper.pendingPcReplacement.
     */
    private void showPcReplacementDialog(final String pendingPc) {
        pcReplacementDialogShowing = true;
        new AlertDialog.Builder(this)
                .setTitle("New computer detected")
                .setMessage("Another computer running Mirryl wants to connect to this "
                        + "device. If this is your new or repaired computer, allow it - "
                        + "this device will stop trusting the old one.")
                .setCancelable(false)
                .setPositiveButton("Allow", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        pcReplacementDialogShowing = false;
                        WifiAdbHelper.acceptPcReplacement(MainActivity.this, pendingPc);
                        refresh();
                    }
                })
                .setNegativeButton("Not now", new DialogInterface.OnClickListener() {
                    @Override
                    public void onClick(DialogInterface dialog, int which) {
                        pcReplacementDialogShowing = false;
                        WifiAdbHelper.dismissPcReplacement(MainActivity.this, pendingPc);
                    }
                })
                .show();
    }

    private void refresh() {
        boolean granted = WifiAdbHelper.hasWriteSecureSettingsPermission(this);
        int permissionColor = granted ? Theme.okText(this) : Theme.errText(this);
        permissionText.setText(granted ? "Permission: Granted" : "Permission: Not granted");
        permissionText.setTextColor(permissionColor);
        permissionDot.setBackground(Theme.dot(this, permissionColor));
        permissionHint.setVisibility(granted ? View.GONE : View.VISIBLE);

        boolean enabled = WifiAdbHelper.isEnabled(this);
        int enabledColor = enabled ? Theme.okText(this) : Theme.errText(this);
        statusText.setText("Wireless debugging: " + (enabled ? "Enabled" : "Disabled"));
        statusText.setTextColor(enabledColor);
        statusDot.setBackground(Theme.dot(this, enabledColor));

        // Without this line, a locked phone just silently never turns the
        // toggle on - which reads as "the app is broken", not "needs Mirryl on a PC".
        // See WifiAdbHelper.hasFreshSeal/isMatriculated for what unlocks this.
        // See WifiAdbHelper.enable(): opening the app is the other moment a
        // phone that never wrote its .devid gets to write it (versionCode 16).
        WifiAdbHelper.publishDeviceId(this);
        boolean unlocked = WifiAdbHelper.hasFreshSeal(this) || WifiAdbHelper.isMatriculated(this);
        secondaryText.setText(unlocked
                ? "WiFi IP: " + WifiAdbHelper.currentIp(this)
                : "Requires Mirryl on a PC. Connect once to unlock.");

        if (!pcReplacementDialogShowing) {
            String pendingPc = WifiAdbHelper.pendingPcReplacement(this);
            if (pendingPc != null) {
                showPcReplacementDialog(pendingPc);
            }
        }

        boolean ignoringOptimizations = WifiAdbHelper.isIgnoringBatteryOptimizations(this);
        batteryBody.setText(ignoringOptimizations
                ? "On - Mirryl on your PC turned this on so this phone reconnects quickly after Wi-Fi drops or while it is idle. You can turn it off from Battery settings; Mirryl on your PC turns it on again the next time it connects."
                : "Off - Mirryl on your PC turns this on the next time it connects. Without it, getting back after Wi-Fi drops can take minutes, or wait until you unlock this phone.");
    }

    private int dp(int value) {
        return Theme.dp(this, value);
    }
}
