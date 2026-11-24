package dev.mirador.agent;

import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.List;

/**
 * The order in which the video agent offers sizes to the encoder.
 *
 * Pure arithmetic, kept apart from {@link VideoStreamMain} so it can be checked
 * on a desktop JVM without an Android runtime.
 *
 * Why the order matters (measured on a 1080x2160 phone with a Snapdragon
 * SM6115): its hardware H.264 encoder DECLARES widths [128, 1920]
 * and heights [128, 1088], refuses {@code configure} at 1080x2160, and yet
 * accepts 1080x1920, 1088x1920 and 960x1920 (each configured, given an input
 * surface and started, by a probe on that phone). Why it accepts them is not
 * known: the vendor codec XML on that phone lists a 1920x1088 maximum with
 * 16x16 blocks, so a block-count limit that ignores orientation would fit the
 * facts, but that is a guess. Trusting the declaration as soon as the first
 * size fails would fit the picture to it: 544x1088, half the resolution, when
 * 960x1920 is there for the asking.
 *
 * So the first three offers are, in this order: the size as asked for; the
 * same picture fitted to the declared limits WITH THE AXES
 * ALLOWED TO SWAP - long side within the larger declared maximum, short side
 * within the smaller one, aspect kept - which on the phone above is exactly
 * the measured 960x1920; and the size fitted to the declared limits as they
 * stand. Only then the ladder rungs, largest
 * area first, each raw before fitted. An encoder that is honest about a
 * landscape-only limit therefore costs two refused {@code configure} calls
 * (roughly 100 ms each as measured on the phone above, once per session)
 * before the size it would have got anyway; one that understates its limits
 * gets the picture it can actually produce on the second call.
 */
final class SizeLadder {
    /**
     * Long edges to step down through, largest first, when the encoder refuses
     * the size it was asked for. Each step keeps the phone's aspect ratio.
     *
     * Why a ladder at all: `wm size` says what the panel measures, not what
     * this phone's H.264 encoder can take. Plenty of cheap and rugged phones
     * carry a 1080x2400 panel and an encoder that stops at 1080p, or one that
     * only takes multiples of 16, and `configure` throws for them. The ladder finds the
     * largest size the encoder actually accepts; the picture is a little
     * smaller on those phones, and exists.
     */
    static final int[] FALLBACK_LONG_EDGES = {
        2160, 1920, 1600, 1440, 1280, 1080, 960, 800, 640,
    };

    /** Shrinks a size to what the encoder declares; identity when it declares nothing. */
    interface Fitter {
        /** The size fitted to the declared width and height ranges as they stand. */
        int[] fit(int width, int height);

        /**
         * The size fitted to the declared ranges with the axes allowed to swap:
         * the long side within the larger declared maximum, the short side
         * within the smaller one. Null when the declaration says nothing.
         */
        int[] fitEitherWay(int width, int height);
    }

    private SizeLadder() {
    }

    /**
     * Every size worth offering, no duplicates. First the three pinned offers
     * described above (requested, fitted either way, fitted as declared), then
     * each ladder rung below the requested long edge at the same aspect ratio
     * and its fitted counterpart, largest area first with the raw size ahead
     * of the fitted one on a tie. Never empty: the requested size is always
     * first.
     */
    static List<int[]> candidates(int width, int height, Fitter fitter) {
        List<int[]> ordered = new ArrayList<>();
        add(ordered, new int[] {width, height});
        if (fitter != null) {
            addIfSane(ordered, fitter.fitEitherWay(width, height));
            addIfSane(ordered, fitter.fit(width, height));
        }
        List<int[]> rungs = new ArrayList<>();
        int longEdge = Math.max(width, height);
        for (int rung : FALLBACK_LONG_EDGES) {
            if (rung >= longEdge) {
                continue;
            }
            int[] scaled = scaleToLongEdge(width, height, rung);
            add(rungs, scaled);
            if (fitter != null) {
                addIfSane(rungs, fitter.fit(scaled[0], scaled[1]));
            }
        }
        // Stable: among equal areas the raw size stays ahead of the fitted one.
        Collections.sort(rungs, new Comparator<int[]>() {
            @Override
            public int compare(int[] a, int[] b) {
                long areaA = (long) a[0] * a[1];
                long areaB = (long) b[0] * b[1];
                return areaA == areaB ? 0 : (areaA > areaB ? -1 : 1);
            }
        });
        for (int[] rung : rungs) {
            add(ordered, rung);
        }
        return ordered;
    }

    private static void addIfSane(List<int[]> ordered, int[] size) {
        if (size != null && size.length == 2 && size[0] >= 2 && size[1] >= 2) {
            add(ordered, size);
        }
    }

    private static void add(List<int[]> ordered, int[] size) {
        for (int[] seen : ordered) {
            if (seen[0] == size[0] && seen[1] == size[1]) {
                return;
            }
        }
        ordered.add(size);
    }

    /** The requested aspect ratio at a smaller long edge, in even numbers. */
    static int[] scaleToLongEdge(int width, int height, int longEdge) {
        boolean wide = width >= height;
        int longSide = wide ? width : height;
        int shortSide = wide ? height : width;
        int scaledShort = (int) ((long) shortSide * longEdge / longSide);
        int newLong = alignDown(longEdge, 2);
        int newShort = Math.max(2, alignDown(scaledShort, 2));
        return wide ? new int[] {newLong, newShort} : new int[] {newShort, newLong};
    }

    static int alignDown(int value, int alignment) {
        return value - (value % alignment);
    }

    static String describe(int[] size) {
        return size[0] + "x" + size[1];
    }
}
