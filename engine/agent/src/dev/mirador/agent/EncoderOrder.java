package dev.mirador.agent;

import java.util.ArrayList;
import java.util.Collection;
import java.util.List;

/**
 * The order in which the phone's H.264 encoders are tried. Pure arithmetic on
 * names and two flags, so it can be checked on the PC's JVM like
 * {@link SizeLadder}; the agent feeds it what MediaCodecList reports.
 *
 * With only the first AVC encoder in the system list, a phone whose encoder
 * refuses every size on the ladder could not mirror at all, even when a second
 * encoder on the same phone (often the software one) would take the very first
 * size. The order below keeps that first encoder first, and only then goes on:
 *
 * 1. the encoder that already worked in this agent, when there is one - a
 *    rotation rebuild should not climb past encoders that refused a moment
 *    ago;
 * 2. the system's first AVC encoder - what createEncoderByType gives;
 * 3. every other hardware encoder, in system order;
 * 4. every software encoder, in system order - last, because they are slower
 *    and some cap the bit rate low, but a picture beats none.
 *
 * Aliases (Android 10+ lists some encoders twice under two names) are left
 * out: trying the same codec twice only doubles the wait before the answer.
 */
final class EncoderOrder {
    private EncoderOrder() {
    }

    /** What MediaCodecList says about one AVC encoder. */
    static final class Entry {
        final String name;
        final boolean software;
        final boolean alias;

        Entry(String name, boolean software, boolean alias) {
            this.name = name;
            this.software = software;
            this.alias = alias;
        }
    }

    /**
     * Software-only by name, for Android 9 and older where MediaCodecInfo
     * cannot say. These are the AOSP software prefixes; a vendor software
     * encoder under another name is taken for hardware, which only moves it
     * earlier - it is still tried.
     */
    static boolean looksSoftware(String name) {
        String lower = name.toLowerCase();
        return lower.startsWith("omx.google.") || lower.startsWith("c2.android.");
    }

    static List<String> order(List<Entry> systemOrder, String alreadyWorked) {
        List<Entry> real = new ArrayList<>();
        for (Entry entry : systemOrder) {
            if (!entry.alias && !containsName(real, entry.name)) {
                real.add(entry);
            }
        }
        List<String> order = new ArrayList<>();
        if (alreadyWorked != null && containsName(real, alreadyWorked)) {
            order.add(alreadyWorked);
        }
        if (!real.isEmpty()) {
            addOnce(order, real.get(0).name);
        }
        for (Entry entry : real) {
            if (!entry.software) {
                addOnce(order, entry.name);
            }
        }
        for (Entry entry : real) {
            if (entry.software) {
                addOnce(order, entry.name);
            }
        }
        return order;
    }

    /** Names from a comma list, trimmed, empties dropped. For the test hook. */
    static List<String> parseList(String value) {
        List<String> names = new ArrayList<>();
        if (value == null) {
            return names;
        }
        for (String part : value.split(",")) {
            String name = part.trim();
            if (!name.isEmpty()) {
                names.add(name);
            }
        }
        return names;
    }

    private static boolean containsName(Collection<Entry> entries, String name) {
        for (Entry entry : entries) {
            if (entry.name.equals(name)) {
                return true;
            }
        }
        return false;
    }

    private static void addOnce(List<String> names, String name) {
        if (!names.contains(name)) {
            names.add(name);
        }
    }
}
