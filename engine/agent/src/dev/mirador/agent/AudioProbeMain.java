package dev.mirador.agent;

import android.media.AudioFormat;
import android.media.AudioRecord;

public final class AudioProbeMain {
    private static final int AUDIO_SOURCE_REMOTE_SUBMIX = 8;
    private static final int SAMPLE_RATE = 48_000;

    private AudioProbeMain() {
    }

    public static void main(String[] arguments) {
        int minimum = AudioRecord.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_STEREO,
                AudioFormat.ENCODING_PCM_16BIT);
        int bufferSize = Math.max(minimum, 16_384);
        System.out.println("minBuffer=" + minimum + " selected=" + bufferSize);

        AudioRecord recorder = null;
        try {
            recorder = new AudioRecord(
                    AUDIO_SOURCE_REMOTE_SUBMIX,
                    SAMPLE_RATE,
                    AudioFormat.CHANNEL_IN_STEREO,
                    AudioFormat.ENCODING_PCM_16BIT,
                    bufferSize);
            System.out.println("state=" + recorder.getState());
            recorder.startRecording();
            System.out.println("recordingState=" + recorder.getRecordingState());

            byte[] buffer = new byte[4096];
            long total = 0;
            long nonZero = 0;
            long deadline = System.nanoTime() + 2_000_000_000L;
            while (System.nanoTime() < deadline) {
                int count = recorder.read(buffer, 0, buffer.length, AudioRecord.READ_BLOCKING);
                if (count < 0) {
                    throw new IllegalStateException("AudioRecord.read returned " + count);
                }
                total += count;
                for (int index = 0; index < count; index++) {
                    if (buffer[index] != 0) {
                        nonZero++;
                    }
                }
            }
            System.out.println("bytes=" + total + " nonZero=" + nonZero);
        } catch (Throwable error) {
            error.printStackTrace(System.err);
            System.exit(1);
        } finally {
            if (recorder != null) {
                try {
                    recorder.stop();
                } catch (Throwable ignored) {
                }
                recorder.release();
            }
        }
    }
}
