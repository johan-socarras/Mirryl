package com.mirryl.app;

import android.graphics.Bitmap;
import android.graphics.BitmapShader;
import android.graphics.Canvas;
import android.graphics.ColorFilter;
import android.graphics.Paint;
import android.graphics.PixelFormat;
import android.graphics.Rect;
import android.graphics.Shader;
import android.graphics.drawable.Drawable;

/**
 * The fine dot texture behind Mirryl's screens - the desktop app's
 * radial-gradient dot (styles.css), tiled every few pixels at low opacity.
 *
 * Built as a single cell bitmap repeated by a BitmapShader, the same way CSS
 * tiles a background image, so covering the screen costs one drawRect instead
 * of the tens of thousands of drawCircle calls it would take otherwise.
 */
final class DotPatternDrawable extends Drawable {

    /** Dot radius as a fraction of the cell. */
    private static final float RADIUS_RATIO = 0.13f;

    private final Paint paint = new Paint();

    DotPatternDrawable(int dotColor, int cellPx) {
        int cell = Math.max(2, cellPx);

        Paint dotPaint = new Paint(Paint.ANTI_ALIAS_FLAG);
        dotPaint.setColor(dotColor);

        Bitmap tile = Bitmap.createBitmap(cell, cell, Bitmap.Config.ARGB_8888);
        new Canvas(tile).drawCircle(cell / 2f, cell / 2f,
                Math.max(1f, cell * RADIUS_RATIO), dotPaint);

        paint.setShader(new BitmapShader(tile, Shader.TileMode.REPEAT, Shader.TileMode.REPEAT));
    }

    @Override
    public void draw(Canvas canvas) {
        Rect bounds = getBounds();
        if (bounds.width() <= 0 || bounds.height() <= 0) {
            return;
        }
        canvas.drawRect(bounds, paint);
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
