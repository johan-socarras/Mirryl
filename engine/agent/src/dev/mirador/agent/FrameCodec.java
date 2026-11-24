package dev.mirador.agent;

import java.io.DataInputStream;
import java.io.DataOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Arrays;

final class FrameCodec {
    static final int VERSION = 1;
    static final int CHANNEL_SESSION = 1;
    static final int KIND_HELLO = 1;
    static final int KIND_HEARTBEAT = 7;
    static final int KIND_STOP = 8;
    static final int KIND_FAULT = 255;
    static final int MAXIMUM_PAYLOAD = 16 * 1024 * 1024;

    private static final byte[] MAGIC = new byte[] {'M', 'R', 'D', 'R'};

    private FrameCodec() {
    }

    static Frame read(InputStream input) throws IOException {
        DataInputStream data = new DataInputStream(input);
        byte[] magic = new byte[4];
        try {
            data.readFully(magic);
        } catch (EOFException error) {
            throw new EOFException("connection closed before frame header");
        }
        if (!Arrays.equals(magic, MAGIC)) {
            throw new IOException("invalid Mirador frame magic");
        }

        int version = data.readUnsignedByte();
        int channel = data.readUnsignedByte();
        int kind = data.readUnsignedByte();
        int flags = data.readUnsignedByte();
        long sequence = Integer.toUnsignedLong(data.readInt());
        long payloadLength = Integer.toUnsignedLong(data.readInt());

        if (version != VERSION) {
            throw new IOException("unsupported protocol version");
        }
        if (payloadLength > MAXIMUM_PAYLOAD) {
            throw new IOException("payload exceeds protocol limit");
        }

        byte[] payload = new byte[(int) payloadLength];
        data.readFully(payload);
        return new Frame(
                version,
                channel,
                kind,
                flags,
                sequence,
                payload);
    }

    static void write(OutputStream output, Frame frame) throws IOException {
        if (frame.version != VERSION) {
            throw new IOException("unsupported protocol version");
        }
        if (frame.payload.length > MAXIMUM_PAYLOAD) {
            throw new IOException("payload exceeds protocol limit");
        }

        DataOutputStream data = new DataOutputStream(output);
        data.write(MAGIC);
        data.writeByte(frame.version);
        data.writeByte(frame.channel);
        data.writeByte(frame.kind);
        data.writeByte(frame.flags);
        data.writeInt((int) frame.sequence);
        data.writeInt(frame.payload.length);
        data.write(frame.payload);
        data.flush();
    }
}

