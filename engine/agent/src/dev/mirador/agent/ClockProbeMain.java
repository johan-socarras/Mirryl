package dev.mirador.agent;

import java.io.DataOutputStream;
import java.io.FileDescriptor;
import java.io.FileOutputStream;

public final class ClockProbeMain {
    private ClockProbeMain() {
    }

    public static void main(String[] arguments) throws Exception {
        DataOutputStream output =
                new DataOutputStream(new FileOutputStream(FileDescriptor.out));
        output.writeLong(System.nanoTime() / 1_000L);
        output.flush();
    }
}
