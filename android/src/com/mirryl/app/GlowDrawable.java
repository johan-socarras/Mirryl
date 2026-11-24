package com.mirryl.app;

import android.graphics.Canvas;
import android.graphics.ColorFilter;
import android.graphics.Paint;
import android.graphics.PixelFormat;
import android.graphics.RadialGradient;
import android.graphics.Rect;
import android.graphics.Shader;
import android.graphics.drawable.Drawable;

/**
 * The coloured corner washes on the landing page background - a violet one in
 * the top left and a cyan one across from it, both very soft and very wide.
 *
 * The site places them with radial-gradient(circle at 15% 20%, ...) over a
 * viewport that is wider than it is tall. Here the centres stay as fractions of
 * the bounds, so each one lands in the same corner on any screen, but the
 * radius is taken as a fraction of the WIDTH rather than of the diagonal: a
 * phone is more than twice as tall as it is wide, and sizing off the diagonal
 * would drag every glow halfway down the screen.
 */
final class GlowDrawable extends Drawable {

    private final float[] centerX;
    private final float[] centerY;
    private final float[] radius;
    private final int[] colors;

    private final Paint paint = new Paint(Paint.ANTI_ALIAS_FLAG);
    private Shader[] shaders;
    private int shadedWidth = -1;
    private int shadedHeight = -1;

    /**
     * Parallel arrays, one entry per glow: centre and radius as fractions of
     * the bounds, and the colour it fades out from.
     */
    GlowDrawable(float[] centerX, float[] centerY, float[] radius, int[] colors) {
        this.centerX = centerX;
        this.centerY = centerY;
        this.radius = radius;
        this.colors = colors;
    }

    @Override
    public void draw(Canvas canvas) {
        Rect bounds = getBounds();
        if (bounds.width() <= 0 || bounds.height() <= 0) {
            return;
        }
        if (bounds.width() != shadedWidth || bounds.height() != shadedHeight) {
            shadedWidth = bounds.width();
            shadedHeight = bounds.height();
            shaders = new Shader[colors.length];
            for (int i = 0; i < colors.length; i++) {
                shaders[i] = new RadialGradient(
                        bounds.left + centerX[i] * bounds.width(),
                        bounds.top + centerY[i] * bounds.height(),
                        Math.max(1f, radius[i] * bounds.width()),
                        colors[i], colors[i] & 0x00FFFFFF,
                        Shader.TileMode.CLAMP);
            }
        }
        for (int i = 0; i < shaders.length; i++) {
            paint.setShader(shaders[i]);
            canvas.drawRect(bounds, paint);
        }
    }

    @Override
    public void setAlpha(int alpha) {
        paint.setAlpha(alpha);
        invalidateSelf();
    }

    @Override
    public void setColorFilter(ColorFilter colorFilter) {
        paint.setColorFilter(colorFilter);
        invalidateSelf();
    }

    @Override
    public int getOpacity() {
        return PixelFormat.TRANSLUCENT;
    }
}
