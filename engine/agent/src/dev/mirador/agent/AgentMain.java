package dev.mirador.agent;

import android.net.LocalServerSocket;
import android.net.LocalSocket;
import android.os.Build;

import java.io.IOException;
import java.util.regex.Pattern;

public final class AgentMain {
    private static final Pattern SESSION_TOKEN =
            Pattern.compile("[A-Za-z0-9]{16,64}");

    private AgentMain() {
    }

    public static void main(String[] arguments) {
        if (arguments.length != 1
                || !SESSION_TOKEN.matcher(arguments[0]).matches()) {
            System.err.println("Mirador agent requires a valid session token.");
            System.exit(2);
            return;
        }

        String socketName = "mirador_" + arguments[0];
        try {
            serve(socketName);
        } catch (Throwable error) {
            System.err.println("Mirador agent stopped: " + error.getMessage());
            error.printStackTrace(System.err);
            System.exit(1);
        }
    }

    private static void serve(String socketName) throws IOException {
        try (LocalServerSocket server = new LocalServerSocket(socketName);
             LocalSocket client = server.accept()) {
            client.setSoTimeout(15_000);

            Frame hostHello = FrameCodec.read(client.getInputStream());
            if (hostHello.channel != FrameCodec.CHANNEL_SESSION
                    || hostHello.kind != FrameCodec.KIND_HELLO) {
                throw new IOException("expected host hello");
            }

            byte[] hello = HelloPayload.encode(
                    HelloPayload.ROLE_ANDROID_AGENT,
                    FrameCodec.VERSION,
                    FrameCodec.VERSION,
                    AgentCapabilities.detect(),
                    Build.VERSION.SDK_INT,
                    Build.MANUFACTURER + " " + Build.MODEL);
            FrameCodec.write(
                    client.getOutputStream(),
                    new Frame(
                            FrameCodec.VERSION,
                            FrameCodec.CHANNEL_SESSION,
                            FrameCodec.KIND_HELLO,
                            0,
                            0,
                            hello));

            while (true) {
                Frame request = FrameCodec.read(client.getInputStream());
                if (request.channel != FrameCodec.CHANNEL_SESSION) {
                    throw new IOException("streaming is not enabled in this build");
                }
                if (request.kind == FrameCodec.KIND_STOP) {
                    return;
                }
                if (request.kind == FrameCodec.KIND_HEARTBEAT) {
                    FrameCodec.write(
                            client.getOutputStream(),
                            new Frame(
                                    FrameCodec.VERSION,
                                    FrameCodec.CHANNEL_SESSION,
                                    FrameCodec.KIND_HEARTBEAT,
                                    0,
                                    request.sequence,
                                    new byte[0]));
                } else {
                    FrameCodec.write(
                            client.getOutputStream(),
                            new Frame(
                                    FrameCodec.VERSION,
                                    FrameCodec.CHANNEL_SESSION,
                                    FrameCodec.KIND_FAULT,
                                    0,
                                    request.sequence,
                                    new byte[0]));
                }
            }
        }
    }
}
