package dev.mirador.agent;

import android.media.MediaCodecInfo;
import android.media.MediaCodecList;

final class AgentCapabilities {
    private AgentCapabilities() {
    }

    static int detect() {
        int flags = 0;
        MediaCodecInfo[] codecs =
                new MediaCodecList(MediaCodecList.ALL_CODECS).getCodecInfos();
        for (MediaCodecInfo codec : codecs) {
            if (!codec.isEncoder()) {
                continue;
            }
            for (String type : codec.getSupportedTypes()) {
                if ("video/avc".equalsIgnoreCase(type)) {
                    flags |= HelloPayload.CAPABILITY_H264_ENCODER;
                    return flags;
                }
            }
        }
        return flags;
    }
}

