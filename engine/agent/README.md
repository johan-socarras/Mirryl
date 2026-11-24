# Mirryl Connect Android agent

This directory contains Mirryl's independently authored Android-side agent. It
does not contain code or artifacts from another mirroring application.

The current source implements:

- A uniquely named local Android socket.
- Mirador protocol header parsing and bounds checking.
- Host/agent hello negotiation.
- H.264 encoder capability detection.
- Heartbeat response and clean stop.
- A direct MediaCodec H.264 stream fed by an Android virtual display.
- A 48 kHz stereo remote-submix PCM stream.

The agent is compiled to dex and packaged in a JAR that is started by the
authorized ADB `shell` user:

```text
CLASSPATH=/data/local/tmp/mirryl-agent.jar \
app_process / dev.mirador.agent.AgentMain <session-token>
```

The stream entry points are compiled with the locally installed Android SDK and
embedded in the engine. Run `scripts/build-agent.ps1` to reproduce
`assets/mirryl-stream-agent.jar`. Mirryl pushes the JAR
temporarily to the authorized device and removes it when the session ends.
