package com.mirryl.app;

import android.app.Activity;
import android.content.Context;
import android.content.SharedPreferences;
import android.content.res.Configuration;
import android.graphics.Color;
import android.graphics.Insets;
import android.graphics.Typeface;
import android.graphics.drawable.ColorDrawable;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.LayerDrawable;
import android.view.Gravity;
import android.view.View;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.widget.Button;
import android.widget.TextView;

/**
 * Palette and shared component styling, the same numbers as the desktop app's
 * CSS custom properties (styles.css) so the phone and the PC read as one
 * product. Anything that decides a colour, a corner radius or a font lives
 * here; the Activities only assemble the views.
 */
final class Theme {

    private static final String PREFS_NAME = "theme_prefs";
    private static final String KEY_MODE = "theme_mode";

    static final int MODE_SYSTEM = 0;
    static final int MODE_LIGHT = 1;
    static final int MODE_DARK = 2;

    private Theme() {
    }

    /** The user's saved choice, or MODE_SYSTEM until they pick one. */
    static int mode(Context context) {
        return prefs(context).getInt(KEY_MODE, MODE_SYSTEM);
    }

    /** Flips between light and dark and persists it across restarts and reboots. */
    static void toggle(Context context) {
        int next = isDark(context) ? MODE_LIGHT : MODE_DARK;
        prefs(context).edit().putInt(KEY_MODE, next).apply();
    }

    static boolean isDark(Context context) {
        int mode = mode(context);
        if (mode == MODE_LIGHT) {
            return false;
        }
        if (mode == MODE_DARK) {
            return true;
        }
        int uiMode = context.getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return uiMode == Configuration.UI_MODE_NIGHT_YES;
    }

    /**
     * Platform theme matching the current palette, so the window background and
     * system dialogs don't flash the opposite theme. Both are NoActionBar: the
     * screens draw their own title.
     */
    static int platformTheme(Context context) {
        return isDark(context)
                ? android.R.style.Theme_DeviceDefault_NoActionBar
                : android.R.style.Theme_DeviceDefault_Light_NoActionBar;
    }

    private static SharedPreferences prefs(Context context) {
        return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE);
    }

    // ---------------------------------------------------------------- colours

    private static int pick(Context context, String dark, String light) {
        return Color.parseColor(isDark(context) ? dark : light);
    }

    static int bg(Context context) {
        return pick(context, "#070914", "#f3f5ff");
    }

    static int card(Context context) {
        return pick(context, "#111525", "#ffffff");
    }

    static int text(Context context) {
        return pick(context, "#f7f8ff", "#101426");
    }

    static int muted(Context context) {
        return pick(context, "#a8aec7", "#616983");
    }

    /** White (or ink) at 12%, exactly like the desktop app's --border. */
    static int border(Context context) {
        return pick(context, "#1FFFFFFF", "#1F171F39");
    }

    static int accent(Context context) {
        return pick(context, "#65e7ff", "#07717f");
    }

    /** Text that sits ON TOP of the accent. White over cyan is unreadable. */
    static int onAccent(Context context) {
        return pick(context, "#07111d", "#ffffff");
    }

    static int okText(Context context) {
        return pick(context, "#86efac", "#15803d");
    }

    static int errText(Context context) {
        return pick(context, "#ff7a8f", "#bd2947");
    }

    /** The two stops of the primary button gradient. */
    private static int[] accentGradient(Context context) {
        return isDark(context)
                ? new int[] { Color.parseColor("#65e7ff"), Color.parseColor("#9b8cff") }
                : new int[] { Color.parseColor("#087f98"), Color.parseColor("#6253db") };
    }

    /** Same hue, new opacity - for tinted fills that follow the accent. */
    private static int withAlpha(int color, float alpha) {
        return Color.argb(Math.round(255 * alpha),
                Color.red(color), Color.green(color), Color.blue(color));
    }

    /** Scales whatever alpha the colour already carries. */
    private static int scaleAlpha(int color, float factor) {
        return Color.argb(Math.round(Color.alpha(color) * factor),
                Color.red(color), Color.green(color), Color.blue(color));
    }

    // ------------------------------------------------------------- background

    /**
     * Flat colour plus the fine dot texture.
     *
     * The desktop app tiles its dot every 5 CSS px. Copying that number
     * straight over would be invisible here - a phone pixel is a fraction of
     * the size of a monitor one and is held twice as close - so the cell is
     * sized in dp to land at the same apparent grain, and the dot keeps its ratio of
     * radius to cell. The colour rides on the border token so the texture
     * inverts by itself in the light theme, where white dots would vanish.
     */
    static Drawable screenBackground(Context context) {
        // The site hardcodes these two washes rather than reading its tokens, so
        // they keep the same hues in both themes. Over a near-white background
        // they turn muddy at full strength, so the light theme gets them softer.
        float strength = isDark(context) ? 1f : 0.5f;
        int violetGlow = withAlpha(Color.parseColor("#7d67ff"), 0.26f * strength);
        int cyanGlow = withAlpha(Color.parseColor("#65e7ff"), 0.22f * strength);

        Drawable[] layers = {
                new ColorDrawable(bg(context)),
                new GlowDrawable(
                        new float[] { 0.12f, 0.90f },
                        new float[] { 0.10f, 0.26f },
                        new float[] { 0.80f, 0.72f },
                        new int[] { violetGlow, cyanGlow }),
                // Above the glows, as in the desktop app: dots over the washes.
                new DotPatternDrawable(scaleAlpha(border(context), 0.7f), dp(context, 4))
        };
        return new LayerDrawable(layers);
    }

    /**
     * Paints the system bars to match the palette, so the app doesn't sit in a
     * grey frame. minSdk is 30, so WindowInsetsController is always available.
     *
     * MUST be called after setContentView: getInsetsController() dereferences
     * the decor view, which does not exist until the content is installed, and
     * it throws from inside the getter rather than returning null.
     */
    static void applySystemBars(Activity activity) {
        int background = bg(activity);
        activity.getWindow().setBackgroundDrawable(new ColorDrawable(background));
        activity.getWindow().setStatusBarColor(background);
        activity.getWindow().setNavigationBarColor(background);

        final boolean dark = isDark(activity);
        final View decor = activity.getWindow().getDecorView();
        // The icon appearance only sticks once the decor view is attached to the
        // window manager. Set straight from onCreate it silently does nothing,
        // and the clock stays white on the light background. Posting runs it
        // after the first traversal, when the
        // controller is live.
        decor.post(new Runnable() {
            @Override
            public void run() {
                WindowInsetsController controller = decor.getWindowInsetsController();
                if (controller == null) {
                    return;
                }
                int lightBars = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                        | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                controller.setSystemBarsAppearance(dark ? 0 : lightBars, lightBars);
            }
        });
    }

    /**
     * Pads a root view by the system bars' own insets, on top of the app's
     * normal padding, and keeps re-applying it on every inset change (e.g.
     * rotating, or the 3-button nav bar showing/hiding). Needed because
     * targeting Android 15+ (API 35+) forces edge-to-edge for every app -
     * content draws behind the status/navigation bars by default, and
     * there's no opting out. Without this, the header row or the bottom
     * buttons would sit under those bars instead of above them. minSdk is
     * 30, so WindowInsets.Type.systemBars() is always available.
     */
    static void applyContentInsets(final View root, final int left, final int top,
                                    final int right, final int bottom) {
        root.setPadding(left, top, right, bottom);
        root.setOnApplyWindowInsetsListener(new View.OnApplyWindowInsetsListener() {
            @Override
            public WindowInsets onApplyWindowInsets(View v, WindowInsets insets) {
                Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
                v.setPadding(left + bars.left, top + bars.top,
                        right + bars.right, bottom + bars.bottom);
                return insets;
            }
        });
    }

    // ------------------------------------------------------------- primitives

    /** Rounded, bordered card, matching the desktop app's .card rule. */
    static GradientDrawable cardBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(card(context));
        drawable.setCornerRadius(dp(context, 18));
        drawable.setStroke(dp(context, 1), border(context));
        return drawable;
    }

    /** Filled gradient button, matching the desktop app's primary button. */
    static GradientDrawable buttonBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setOrientation(GradientDrawable.Orientation.TL_BR);
        drawable.setColors(accentGradient(context));
        drawable.setCornerRadius(dp(context, 14));
        return drawable;
    }

    /** Outlined button, for anything that must not compete with the primary. */
    static GradientDrawable ghostButtonBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(Color.TRANSPARENT);
        drawable.setCornerRadius(dp(context, 14));
        drawable.setStroke(dp(context, 1), border(context));
        return drawable;
    }

    /** Fully rounded outline, for status pills and the Close button. */
    static GradientDrawable pillBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(Color.TRANSPARENT);
        drawable.setCornerRadius(dp(context, 999));
        drawable.setStroke(dp(context, 1), border(context));
        return drawable;
    }

    /** Circular surface, for the icon-only buttons in the headers. */
    static GradientDrawable circleBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setShape(GradientDrawable.OVAL);
        drawable.setColor(card(context));
        drawable.setStroke(dp(context, 1), border(context));
        return drawable;
    }

    /** Accent-tinted tile that replaces the emoji on the "finish setup" card. */
    static GradientDrawable tileBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(withAlpha(accent(context), 0.13f));
        drawable.setCornerRadius(dp(context, 13));
        drawable.setStroke(dp(context, 1), withAlpha(accent(context), 0.26f));
        return drawable;
    }

    /** Faint accent chip. */
    static GradientDrawable chipBackground(Context context) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(withAlpha(accent(context), 0.09f));
        drawable.setCornerRadius(dp(context, 9));
        drawable.setStroke(dp(context, 1), withAlpha(accent(context), 0.20f));
        return drawable;
    }

    static GradientDrawable dot(Context context, int color) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setShape(GradientDrawable.OVAL);
        drawable.setColor(color);
        return drawable;
    }

    /** Progress dot: a circle at 6dp, a stadium once it is widened. */
    static GradientDrawable dotBar(Context context, int color) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(color);
        drawable.setCornerRadius(dp(context, 3));
        return drawable;
    }

    // ------------------------------------------------------------------ text

    /**
     * Section label: monospaced, upper case and tracked out, the same treatment
     * the desktop app gives its eyebrows.
     */
    static void labelStyle(TextView view) {
        view.setTypeface(Typeface.MONOSPACE);
        view.setAllCaps(true);
        view.setTextSize(11);
        view.setLetterSpacing(0.12f);
        view.setTextColor(muted(view.getContext()));
    }

    // --------------------------------------------------------------- buttons

    private static Button baseButton(Context context, String label) {
        Button button = new Button(context);
        button.setText(label);
        button.setAllCaps(false);
        button.setTextSize(15);
        button.setGravity(Gravity.CENTER);
        button.setPadding(dp(context, 12), dp(context, 11), dp(context, 12), dp(context, 11));
        // DeviceDefault animates elevation on press, which shows as a shadow
        // artefact once the background is a flat custom drawable.
        button.setStateListAnimator(null);
        return button;
    }

    static Button primaryButton(Context context, String label) {
        Button button = baseButton(context, label);
        button.setBackground(buttonBackground(context));
        button.setTextColor(onAccent(context));
        button.setTypeface(Typeface.DEFAULT_BOLD);
        return button;
    }

    static Button ghostButton(Context context, String label) {
        Button button = baseButton(context, label);
        button.setBackground(ghostButtonBackground(context));
        button.setTextColor(text(context));
        return button;
    }

    /** Small icon-only round button (theme toggle, refresh). */
    static Button iconButton(Context context, String glyph, int size) {
        Button button = new Button(context);
        button.setText(glyph);
        button.setAllCaps(false);
        button.setTextSize(15);
        button.setGravity(Gravity.CENTER);
        button.setTextColor(accent(context));
        button.setBackground(circleBackground(context));
        button.setStateListAnimator(null);
        button.setPadding(0, 0, 0, 0);
        button.setMinWidth(0);
        button.setMinimumWidth(0);
        button.setMinHeight(0);
        button.setMinimumHeight(0);
        button.setLayoutParams(new android.widget.LinearLayout.LayoutParams(
                dp(context, size), dp(context, size)));
        return button;
    }

    static int dp(Context context, int value) {
        float density = context.getResources().getDisplayMetrics().density;
        return Math.round(value * density);
    }
}
