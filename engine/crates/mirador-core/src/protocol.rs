use std::fmt;
use std::io::{self, Read, Write};

pub const HEADER_LENGTH: usize = 16;
pub const MAXIMUM_PAYLOAD_LENGTH: u32 = 16 * 1024 * 1024;
pub const CURRENT_MAJOR_VERSION: u8 = 1;
const MAGIC: [u8; 4] = *b"MRDR";
pub const VIDEO_PACKET_HEADER_LENGTH: usize = 24;
pub const VIDEO_PACKET_VERSION: u8 = 1;
pub const VIDEO_PACKET_FLAG_CONFIGURATION: u8 = 1;
pub const VIDEO_PACKET_FLAG_KEYFRAME: u8 = 2;
const VIDEO_PACKET_MAGIC: [u8; 4] = *b"MYV1";

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u8)]
pub enum Channel {
    Session = 1,
    Video = 2,
    Audio = 3,
    Control = 4,
}

impl TryFrom<u8> for Channel {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Session),
            2 => Ok(Self::Video),
            3 => Ok(Self::Audio),
            4 => Ok(Self::Control),
            _ => Err(ProtocolError::UnknownChannel(value)),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageKind {
    Hello = 1,
    Capabilities = 2,
    MediaConfiguration = 3,
    MediaPayload = 4,
    Pointer = 5,
    Key = 6,
    Heartbeat = 7,
    Stop = 8,
    Fault = 255,
}

impl TryFrom<u8> for MessageKind {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Capabilities),
            3 => Ok(Self::MediaConfiguration),
            4 => Ok(Self::MediaPayload),
            5 => Ok(Self::Pointer),
            6 => Ok(Self::Key),
            7 => Ok(Self::Heartbeat),
            8 => Ok(Self::Stop),
            255 => Ok(Self::Fault),
            _ => Err(ProtocolError::UnknownMessageKind(value)),
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct FrameHeader {
    pub major_version: u8,
    pub channel: Channel,
    pub kind: MessageKind,
    pub flags: u8,
    pub sequence: u32,
    pub payload_length: u32,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Frame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct VideoPacketHeader {
    pub flags: u8,
    pub payload_length: u32,
    pub sequence: u32,
    pub presentation_time_us: u64,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProtocolError {
    InvalidLength(usize),
    InvalidMagic,
    UnsupportedVersion(u8),
    UnknownChannel(u8),
    UnknownMessageKind(u8),
    PayloadTooLarge(u32),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength(length) => {
                write!(
                    formatter,
                    "header must be {HEADER_LENGTH} bytes, got {length}"
                )
            }
            Self::InvalidMagic => write!(formatter, "invalid Mirador frame magic"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported protocol version {version}")
            }
            Self::UnknownChannel(channel) => write!(formatter, "unknown channel {channel}"),
            Self::UnknownMessageKind(kind) => {
                write!(formatter, "unknown message kind {kind}")
            }
            Self::PayloadTooLarge(length) => {
                write!(formatter, "payload length {length} exceeds limit")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

pub fn encode_header(header: FrameHeader) -> Result<[u8; HEADER_LENGTH], ProtocolError> {
    validate(&header)?;
    let mut output = [0_u8; HEADER_LENGTH];
    output[..4].copy_from_slice(&MAGIC);
    output[4] = header.major_version;
    output[5] = header.channel as u8;
    output[6] = header.kind as u8;
    output[7] = header.flags;
    output[8..12].copy_from_slice(&header.sequence.to_be_bytes());
    output[12..16].copy_from_slice(&header.payload_length.to_be_bytes());
    Ok(output)
}

pub fn decode_header(bytes: &[u8]) -> Result<FrameHeader, ProtocolError> {
    if bytes.len() != HEADER_LENGTH {
        return Err(ProtocolError::InvalidLength(bytes.len()));
    }
    if bytes[..4] != MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }

    let header = FrameHeader {
        major_version: bytes[4],
        channel: Channel::try_from(bytes[5])?,
        kind: MessageKind::try_from(bytes[6])?,
        flags: bytes[7],
        sequence: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
        payload_length: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
    };
    validate(&header)?;
    Ok(header)
}

pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    if frame.payload.len() != frame.header.payload_length as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "payload length does not match frame header",
        ));
    }
    let header = encode_header(frame.header)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    writer.write_all(&header)?;
    writer.write_all(&frame.payload)
}

pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Frame> {
    let mut header_bytes = [0_u8; HEADER_LENGTH];
    reader.read_exact(&mut header_bytes)?;
    let header = decode_header(&header_bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut payload = vec![0_u8; header.payload_length as usize];
    reader.read_exact(&mut payload)?;
    Ok(Frame { header, payload })
}

pub fn encode_video_packet_header(
    header: VideoPacketHeader,
) -> Result<[u8; VIDEO_PACKET_HEADER_LENGTH], ProtocolError> {
    if header.payload_length > MAXIMUM_PAYLOAD_LENGTH {
        return Err(ProtocolError::PayloadTooLarge(header.payload_length));
    }
    let mut output = [0_u8; VIDEO_PACKET_HEADER_LENGTH];
    output[..4].copy_from_slice(&VIDEO_PACKET_MAGIC);
    output[4] = VIDEO_PACKET_VERSION;
    output[5] = header.flags;
    output[6..8].copy_from_slice(&(VIDEO_PACKET_HEADER_LENGTH as u16).to_be_bytes());
    output[8..12].copy_from_slice(&header.payload_length.to_be_bytes());
    output[12..16].copy_from_slice(&header.sequence.to_be_bytes());
    output[16..24].copy_from_slice(&header.presentation_time_us.to_be_bytes());
    Ok(output)
}

pub fn decode_video_packet_header(bytes: &[u8]) -> Result<VideoPacketHeader, ProtocolError> {
    if bytes.len() != VIDEO_PACKET_HEADER_LENGTH {
        return Err(ProtocolError::InvalidLength(bytes.len()));
    }
    if bytes[..4] != VIDEO_PACKET_MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }
    if bytes[4] != VIDEO_PACKET_VERSION {
        return Err(ProtocolError::UnsupportedVersion(bytes[4]));
    }
    let header_length = u16::from_be_bytes(bytes[6..8].try_into().unwrap()) as usize;
    if header_length != VIDEO_PACKET_HEADER_LENGTH {
        return Err(ProtocolError::InvalidLength(header_length));
    }
    let payload_length = u32::from_be_bytes(bytes[8..12].try_into().unwrap());
    if payload_length > MAXIMUM_PAYLOAD_LENGTH {
        return Err(ProtocolError::PayloadTooLarge(payload_length));
    }
    Ok(VideoPacketHeader {
        flags: bytes[5],
        payload_length,
        sequence: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
        presentation_time_us: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
    })
}

fn validate(header: &FrameHeader) -> Result<(), ProtocolError> {
    if header.major_version != CURRENT_MAJOR_VERSION {
        return Err(ProtocolError::UnsupportedVersion(header.major_version));
    }
    if header.payload_length > MAXIMUM_PAYLOAD_LENGTH {
        return Err(ProtocolError::PayloadTooLarge(header.payload_length));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> FrameHeader {
        FrameHeader {
            major_version: CURRENT_MAJOR_VERSION,
            channel: Channel::Video,
            kind: MessageKind::MediaPayload,
            flags: 1,
            sequence: 42,
            payload_length: 4096,
        }
    }

    #[test]
    fn header_round_trips() {
        let expected = sample_header();
        let encoded = encode_header(expected).unwrap();
        assert_eq!(decode_header(&encoded).unwrap(), expected);
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut encoded = encode_header(sample_header()).unwrap();
        encoded[0] ^= 0xff;
        assert_eq!(decode_header(&encoded), Err(ProtocolError::InvalidMagic));
    }

    #[test]
    fn rejects_oversized_payload() {
        let mut header = sample_header();
        header.payload_length = MAXIMUM_PAYLOAD_LENGTH + 1;
        assert_eq!(
            encode_header(header),
            Err(ProtocolError::PayloadTooLarge(MAXIMUM_PAYLOAD_LENGTH + 1))
        );
    }

    #[test]
    fn framed_stream_round_trips() {
        let expected = Frame {
            header: FrameHeader {
                payload_length: 5,
                ..sample_header()
            },
            payload: b"hello".to_vec(),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &expected).unwrap();
        let actual = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn rejects_mismatched_payload_length() {
        let frame = Frame {
            header: FrameHeader {
                payload_length: 10,
                ..sample_header()
            },
            payload: b"short".to_vec(),
        };
        assert_eq!(
            write_frame(&mut Vec::new(), &frame).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn video_packet_header_round_trips_real_pts() {
        let expected = VideoPacketHeader {
            flags: VIDEO_PACKET_FLAG_KEYFRAME,
            payload_length: 123_456,
            sequence: 99,
            presentation_time_us: 8_765_432_100,
        };
        let encoded = encode_video_packet_header(expected).unwrap();
        assert_eq!(decode_video_packet_header(&encoded).unwrap(), expected);
    }

    #[test]
    fn video_packet_header_rejects_wrong_declared_size() {
        let mut encoded = encode_video_packet_header(VideoPacketHeader {
            flags: VIDEO_PACKET_FLAG_CONFIGURATION,
            payload_length: 4,
            sequence: 0,
            presentation_time_us: 0,
        })
        .unwrap();
        encoded[7] = 23;
        assert_eq!(
            decode_video_packet_header(&encoded),
            Err(ProtocolError::InvalidLength(23))
        );
    }
}
