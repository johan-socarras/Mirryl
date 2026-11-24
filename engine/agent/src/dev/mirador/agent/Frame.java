package dev.mirador.agent;

final class Frame {
    final int version;
    final int channel;
    final int kind;
    final int flags;
    final long sequence;
    final byte[] payload;

    Frame(
            int version,
            int channel,
            int kind,
            int flags,
            long sequence,
            byte[] payload) {
        this.version = version;
        this.channel = channel;
        this.kind = kind;
        this.flags = flags;
        this.sequence = sequence;
        this.payload = payload;
    }
}

