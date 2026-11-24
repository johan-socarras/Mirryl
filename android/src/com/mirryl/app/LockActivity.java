package com.mirryl.app;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.graphics.Typeface;
import android.os.Bundle;
import android.text.InputType;
import android.util.TypedValue;
import android.view.Gravity;
import android.view.KeyEvent;
import android.view.View;
import android.view.inputmethod.EditorInfo;
import android.widget.Button;
import android.widget.EditText;
import android.widget.LinearLayout;
import android.widget.TextView;

/**
 * Asks for the PIN before a protected app can be used. Right PIN: this screen
 * goes and the app behind it stays open. Back, or leaving: the home screen.
 */
public class LockActivity extends Activity {
    static final String EXTRA_PACKAGE = "package";

    private String pkg = "";
    private EditText pin;
    private TextView message;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        setTheme(Theme.platformTheme(this));
        super.onCreate(savedInstanceState);
        readPackage(getIntent());
        setContentView(buildLayout());
        Theme.applySystemBars(this);
        // The keyboard comes up with the screen: typing the PIN is all there is to do.
        getWindow().setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE);
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        readPackage(intent);
        setContentView(buildLayout());
    }

    private void readPackage(Intent intent) {
        String extra = intent == null ? null : intent.getStringExtra(EXTRA_PACKAGE);
        pkg = extra == null ? "" : extra;
    }

    private String appLabel() {
        try {
            PackageManager pm = getPackageManager();
            ApplicationInfo info = pm.getApplicationInfo(pkg, 0);
            CharSequence label = pm.getApplicationLabel(info);
            if (label != null && label.length() > 0) {
                return label.toString();
            }
        } catch (Exception ignored) {
            // Not visible to this app: the generic name below.
        }
        return "This app";
    }

    private int dp(float value) {
        return Math.round(TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value, getResources().getDisplayMetrics()));
    }

    private View buildLayout() {
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setGravity(Gravity.CENTER);
        root.setBackground(Theme.screenBackground(this));
        root.setPadding(dp(24), dp(24), dp(24), dp(24));

        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setBackground(Theme.cardBackground(this));
        card.setPadding(dp(24), dp(24), dp(24), dp(24));
        root.addView(card, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT));

        TextView title = new TextView(this);
        title.setText(appLabel() + " is protected");
        title.setTextColor(Theme.text(this));
        title.setTextSize(TypedValue.COMPLEX_UNIT_SP, 20);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        card.addView(title);

        TextView body = new TextView(this);
        body.setText("Type the PIN to open it.");
        body.setTextColor(Theme.muted(this));
        body.setTextSize(TypedValue.COMPLEX_UNIT_SP, 14);
        body.setPadding(0, dp(6), 0, dp(16));
        card.addView(body);

        pin = new EditText(this);
        pin.setInputType(InputType.TYPE_CLASS_NUMBER | InputType.TYPE_NUMBER_VARIATION_PASSWORD);
        pin.setHint("PIN");
        pin.setTextColor(Theme.text(this));
        pin.setHintTextColor(Theme.muted(this));
        pin.setImeOptions(EditorInfo.IME_ACTION_DONE);
        pin.setOnEditorActionListener(new TextView.OnEditorActionListener() {
            @Override
            public boolean onEditorAction(TextView view, int actionId, KeyEvent event) {
                tryPin();
                return true;
            }
        });
        card.addView(pin);

        message = new TextView(this);
        message.setTextColor(Theme.errText(this));
        message.setTextSize(TypedValue.COMPLEX_UNIT_SP, 13);
        message.setPadding(0, dp(8), 0, dp(8));
        card.addView(message);

        Button open = new Button(this);
        open.setText("Open");
        open.setAllCaps(false);
        open.setTextColor(Theme.onAccent(this));
        open.setBackground(Theme.buttonBackground(this));
        open.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                tryPin();
            }
        });
        card.addView(open, new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(48)));

        Button leave = new Button(this);
        leave.setText("Go back");
        leave.setAllCaps(false);
        leave.setTextColor(Theme.text(this));
        leave.setBackground(Theme.ghostButtonBackground(this));
        leave.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View view) {
                goHome();
            }
        });
        LinearLayout.LayoutParams leaveParams = new LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(48));
        leaveParams.topMargin = dp(10);
        card.addView(leave, leaveParams);

        pin.requestFocus();
        return root;
    }

    private void tryPin() {
        if (pin.getText().length() == 0) {
            message.setText("Type the PIN first.");
            return;
        }
        long wait = AppLock.secondsToWait();
        if (wait > 0) {
            message.setText("Too many wrong PINs. Try again in " + wait + " s.");
            return;
        }
        if (AppLock.checkPin(this, pin.getText().toString())) {
            AppLock.unlocked(pkg);
            finish();
        } else {
            pin.setText("");
            wait = AppLock.secondsToWait();
            message.setText(wait > 0 ? "Too many wrong PINs. Try again in " + wait + " s." : "That is not the PIN.");
        }
    }

    private void goHome() {
        Intent home = new Intent(Intent.ACTION_MAIN);
        home.addCategory(Intent.CATEGORY_HOME);
        home.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        startActivity(home);
        finish();
    }

    @Override
    public void onBackPressed() {
        goHome();
    }
}
