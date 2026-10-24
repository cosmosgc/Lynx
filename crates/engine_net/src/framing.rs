//! Length-prefixed framing for envelope streams.
//!
//! A frame is a four-byte big-endian length followed by that many bytes of
//! compact JSON containing exactly one [`Envelope`]. The prefix makes message
//! boundaries explicit on byte streams such as TCP; it is not a replacement
//! for envelope validation.
//!
//! The codec is deliberately boring and transport-neutral. It performs no
//! socket I/O itself: [`read_frame`] and [`write_frame`] work on any
//! [`Read`]/[`Write`], while [`encode_frame`]/[`decode_frame`] expose the same
//! bytes to asynchronous or batched transports.

use std::io::{self, Read, Write};

use engine_core::error::{EngineError, ErrorCode};

use crate::envelope::{Envelope, MAX_MESSAGE_BYTES};

/// Size of the big-endian length prefix, in bytes.
pub const FRAME_HEADER_BYTES: usize = 4;

/// Largest accepted frame payload, in bytes.
///
/// This matches [`MAX_MESSAGE_BYTES`] so framing cannot admit a message that
/// envelope validation would reject anyway.
pub const MAX_FRAME_BYTES: usize = MAX_MESSAGE_BYTES;

/// Encode one envelope as a complete length-prefixed frame.
///
/// # Errors
///
/// Returns the same errors as [`Envelope::encode`]. A body that cannot be
/// represented as a frame is a caller bug, not a transport failure.
pub fn encode_frame(envelope: &Envelope) -> Result<Vec<u8>, EngineError> {
    let text = envelope.encode()?;
    let length = u32::try_from(text.len()).map_err(|_| {
        EngineError::new(ErrorCode::CapacityExceeded, "frame exceeds the size limit")
            .with_detail("bytes", text.len().to_string())
            .with_detail("max_bytes", MAX_FRAME_BYTES.to_string())
    })?;
    let mut frame = Vec::with_capacity(FRAME_HEADER_BYTES + text.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(text.as_bytes());
    Ok(frame)
}

/// Decode exactly one length-prefixed frame.
///
/// # Errors
///
/// Returns [`ErrorCode::InvalidRequest`] for a truncated header, a length that
/// does not match the buffer, invalid UTF-8, or malformed envelope JSON, and
/// [`ErrorCode::CapacityExceeded`] for a zero-length or oversized frame.
pub fn decode_frame(frame: &[u8]) -> Result<Envelope, EngineError> {
    if frame.len() < FRAME_HEADER_BYTES {
        return Err(invalid("frame is shorter than its header"));
    }
    let header: [u8; FRAME_HEADER_BYTES] = frame[..FRAME_HEADER_BYTES]
        .try_into()
        .expect("the header slice has a fixed length");
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(
            EngineError::new(ErrorCode::CapacityExceeded, "frame length is out of range")
                .with_detail("bytes", length.to_string())
                .with_detail("max_bytes", MAX_FRAME_BYTES.to_string()),
        );
    }
    if frame.len() != FRAME_HEADER_BYTES + length {
        return Err(invalid("frame length does not match the buffer"));
    }
    let text = std::str::from_utf8(&frame[FRAME_HEADER_BYTES..])
        .map_err(|error| invalid(&format!("frame is not valid UTF-8: {error}")))?;
    Envelope::decode(text)
}

/// Write one frame and flush the writer.
///
/// # Errors
///
/// Returns an I/O error when the writer fails, and
/// [`io::ErrorKind::InvalidData`] when the envelope cannot be encoded.
pub fn write_frame<W: Write>(writer: &mut W, envelope: &Envelope) -> io::Result<()> {
    let frame = encode_frame(envelope).map_err(|error| invalid_data(&error))?;
    writer.write_all(&frame)?;
    writer.flush()
}

/// Read one frame, returning `Ok(None)` only on a clean end of stream.
///
/// A truncated header or body is an error, not a clean close, so a peer cannot
/// make a partial message look like a deliberate disconnect.
///
/// # Errors
///
/// Returns an I/O error when the reader fails, the stream ends mid-frame, the
/// length prefix is out of range, or the payload is not a valid envelope.
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Option<Envelope>> {
    let mut header = [0_u8; FRAME_HEADER_BYTES];
    let mut filled = 0;
    while filled < FRAME_HEADER_BYTES {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(truncated("frame header")),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }

    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {length} is out of range"),
        ));
    }
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => truncated("frame body"),
            _ => error,
        })?;
    let text = std::str::from_utf8(&body).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame is not valid UTF-8: {error}"),
        )
    })?;
    Envelope::decode(text)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

fn invalid(message: &str) -> EngineError {
    EngineError::new(ErrorCode::InvalidRequest, message)
}

fn invalid_data(error: &EngineError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn truncated(part: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::UnexpectedEof,
        format!("stream ended in the middle of a {part}"),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::envelope::{MessageId, MessageKind};

    use super::*;

    fn envelope(kind: &str, id: &str) -> Envelope {
        Envelope::new(
            MessageId::parse(id).unwrap(),
            MessageKind::parse(kind).unwrap(),
            serde_json::json!({ "hello": "world" }),
        )
    }

    #[test]
    fn frames_round_trip_in_sequence() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &envelope("action.request", "req_1")).unwrap();
        write_frame(&mut bytes, &envelope("session.ack", "ack_1")).unwrap();

        let mut cursor = Cursor::new(bytes);
        let first = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(first.id.as_str(), "req_1");
        assert_eq!(first.kind.as_str(), "action.request");
        let second = read_frame(&mut cursor).unwrap().unwrap();
        assert_eq!(second.id.as_str(), "ack_1");
        assert!(read_frame(&mut cursor).unwrap().is_none());
    }

    #[test]
    fn encoded_frames_decode_one_at_a_time() {
        let frame = encode_frame(&envelope("world.delta", "srv_1")).unwrap();
        let decoded = decode_frame(&frame).unwrap();
        assert_eq!(decoded.kind.as_str(), "world.delta");
        assert_eq!(decoded.body["hello"], "world");
    }

    #[test]
    fn truncated_headers_and_bodies_are_errors() {
        let frame = encode_frame(&envelope("world.delta", "srv_2")).unwrap();

        let mut short_header = Cursor::new(frame[..2].to_vec());
        assert_eq!(
            read_frame(&mut short_header).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut short_body = Cursor::new(frame[..frame.len() - 1].to_vec());
        assert_eq!(
            read_frame(&mut short_body).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn invalid_lengths_and_payloads_are_rejected() {
        assert_eq!(
            read_frame(&mut Cursor::new(0_u32.to_be_bytes().to_vec()))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            read_frame(&mut Cursor::new(
                u32::try_from(MAX_FRAME_BYTES + 1)
                    .unwrap()
                    .to_be_bytes()
                    .to_vec()
            ))
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );

        let mut frame = Vec::new();
        frame.extend_from_slice(&5_u32.to_be_bytes());
        frame.extend_from_slice(b"nope!");
        assert!(decode_frame(&frame).is_err());

        let mut invalid_utf8 = Vec::new();
        invalid_utf8.extend_from_slice(&2_u32.to_be_bytes());
        invalid_utf8.extend_from_slice(&[0xff, 0xfe]);
        assert!(decode_frame(&invalid_utf8).is_err());
    }

    #[test]
    fn a_mismatched_length_prefix_is_rejected() {
        let frame = encode_frame(&envelope("world.delta", "srv_3")).unwrap();
        let mut extra = frame.clone();
        extra.push(b' ');
        assert!(decode_frame(&extra).is_err());
        assert!(decode_frame(&frame[..frame.len() - 1]).is_err());
    }
}
