package dev.mirador.agent;

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;

final class HelloPayload {
    static final int ROLE_ANDROID_AGENT = 1;
    static final int CAPABILITY_H264_ENCODER = 1;

    private HelloPayload() {
    }

    static byte[] encode(
            int role,
            int minimumVersion,
            int maximumVersion,
            int capabilities,
            int androidSdk,
            String deviceName) throws IOException {
        byte[] nameBytes = deviceName.getBytes(StandardCharsets.UTF_8);
        if (nameBytes.length > 1024) {
            throw new IOException("device name is too long");
        }

        ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        DataOutputStream data = new DataOutputStream(buffer);
        data.writeByte(role);
        data.writeByte(minimumVersion);
        data.writeByte(maximumVersion);
        data.writeByte(capabilities);
        data.writeInt(androidSdk);
        data.writeShort(nameBytes.length);
        data.write(nameBytes);
        data.flush();
        return buffer.toByteArray();
    }
}

