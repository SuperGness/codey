// Minimal RFC 6455 WebSocket server-side frame codec over an upgraded TCP
// stream (port of proxy/lib/ws.js). Text frames deliver UTF-8 strings, binary
// frames are rejected by the Responses contract, pings are answered.
use base64::Engine;
use sha1::Digest;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub fn accept_key(client_key: &str) -> String {
    let mut hasher = sha1::Sha1::new();
    hasher.update(format!("{client_key}{WS_GUID}").as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

fn write_frame(buf: &mut Vec<u8>, opcode: u8, payload: &[u8]) {
    buf.push(0x80 | opcode); // FIN + opcode
    let len = payload.len();
    if len < 126 {
        buf.push(len as u8);
    } else if len <= u16::MAX as usize {
        buf.push(126);
        buf.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        buf.push(127);
        buf.extend_from_slice(&(len as u64).to_be_bytes());
    }
    buf.extend_from_slice(payload);
}

pub struct WsError {
    pub code: u16,
    pub reason: String,
}

pub enum WsEvent {
    Text(String),
    Ping(Vec<u8>),
    Pong(Vec<u8>),
    Closed(u16, String),
}

pub struct WsReader<R: AsyncRead + Unpin> {
    reader: R,
    buffer: Vec<u8>,
    /// fragmentation reassembly
    fragments: Vec<u8>,
    frag_opcode: u8,
}

impl<R: AsyncRead + Unpin> WsReader<R> {
    pub fn new(reader: R) -> WsReader<R> {
        WsReader {
            reader,
            buffer: Vec::new(),
            fragments: Vec::new(),
            frag_opcode: 0,
        }
    }

    async fn fill(&mut self) -> Result<usize, std::io::Error> {
        let mut chunk = [0u8; 16384];
        let n = self.reader.read(&mut chunk).await?;
        self.buffer.extend_from_slice(&chunk[..n]);
        Ok(n)
    }

    /// Read the next event. Err(WsError) with code 1002 on protocol errors.
    pub async fn next_event(&mut self) -> Result<WsEvent, WsError> {
        loop {
            let buffered = self.buffer.len();
            if let Some(ev) = self.parse_one()? {
                return Ok(ev);
            }
            if self.buffer.len() < buffered {
                continue;
            }
            let n = self.fill().await.map_err(|_| WsError {
                code: 1006,
                reason: "connection reset".into(),
            })?;
            if n == 0 {
                return Ok(WsEvent::Closed(1006, "connection closed".into()));
            }
        }
    }

    fn parse_one(&mut self) -> Result<Option<WsEvent>, WsError> {
        let need_header = 2usize;
        if self.buffer.len() < need_header {
            return Ok(None);
        }
        let b0 = self.buffer[0];
        let b1 = self.buffer[1];
        let fin = b0 & 0x80 != 0;
        let rsv = b0 & 0x70;
        let opcode = b0 & 0x0F;
        let masked = b1 & 0x80 != 0;
        let len7 = (b1 & 0x7F) as usize;
        if rsv != 0 {
            return Err(WsError {
                code: 1002,
                reason: "protocol error".into(),
            });
        }
        if !masked {
            return Err(WsError {
                code: 1002,
                reason: "protocol error: client frames must be masked".into(),
            });
        }
        let (len, header_len) = if len7 < 126 {
            (len7, 2usize)
        } else if len7 == 126 {
            if self.buffer.len() < 4 {
                return Ok(None);
            }
            (
                u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as usize,
                4usize,
            )
        } else {
            if self.buffer.len() < 10 {
                return Ok(None);
            }
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&self.buffer[2..10]);
            (u64::from_be_bytes(bytes) as usize, 10usize)
        };
        if len > MAX_MESSAGE_BYTES {
            return Err(WsError {
                code: 1009,
                reason: "message too big".into(),
            });
        }
        if opcode >= 0x8 && (!fin || len > 125) {
            return Err(WsError {
                code: 1002,
                reason: "invalid control frame".into(),
            });
        }
        if (opcode == 0x1 || opcode == 0x2) && self.frag_opcode != 0 {
            return Err(WsError {
                code: 1002,
                reason: "expected continuation frame".into(),
            });
        }
        if matches!(opcode, 0x0 | 0x1 | 0x2)
            && self.fragments.len().saturating_add(len) > MAX_MESSAGE_BYTES
        {
            return Err(WsError {
                code: 1009,
                reason: "message too big".into(),
            });
        }
        let total = header_len + 4 + len;
        if self.buffer.len() < total {
            return Ok(None);
        }
        let mask_key = [
            self.buffer[header_len],
            self.buffer[header_len + 1],
            self.buffer[header_len + 2],
            self.buffer[header_len + 3],
        ];
        let mut payload = self.buffer[header_len + 4..total].to_vec();
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask_key[i % 4];
        }
        self.buffer.drain(..total);

        match opcode {
            0x8 => {
                // Close: body carries code + reason (echo verbatim on reply).
                let (code, reason) = if payload.len() >= 2 {
                    (
                        u16::from_be_bytes([payload[0], payload[1]]),
                        String::from_utf8_lossy(&payload[2..]).to_string(),
                    )
                } else {
                    (1005, String::new())
                };
                Ok(Some(WsEvent::Closed(code, reason)))
            }
            0x9 => Ok(Some(WsEvent::Ping(payload))),
            0xA => Ok(Some(WsEvent::Pong(payload))),
            0x1 | 0x2 | 0x0 => {
                if opcode == 0x0 && self.frag_opcode == 0 {
                    return Err(WsError {
                        code: 1002,
                        reason: "protocol error: unexpected continuation".into(),
                    });
                }
                self.fragments.extend_from_slice(&payload);
                if self.fragments.len() > MAX_MESSAGE_BYTES {
                    return Err(WsError {
                        code: 1009,
                        reason: "message too big".into(),
                    });
                }
                if !fin {
                    if self.frag_opcode == 0 {
                        self.frag_opcode = opcode;
                    }
                    return Ok(None);
                }
                let full_opcode = if self.frag_opcode != 0 {
                    self.frag_opcode
                } else {
                    opcode
                };
                self.frag_opcode = 0;
                let message = std::mem::take(&mut self.fragments);
                match full_opcode {
                    0x1 => {
                        let text = String::from_utf8(message).map_err(|_| WsError {
                            code: 1007,
                            reason: "invalid utf-8".into(),
                        })?;
                        Ok(Some(WsEvent::Text(text)))
                    }
                    0x2 => Err(WsError {
                        code: 1003,
                        reason: "binary frames not supported".into(),
                    }),
                    _ => Ok(None),
                }
            }
            _ => Err(WsError {
                code: 1002,
                reason: "protocol error: unknown opcode".into(),
            }),
        }
    }
}

pub struct WsWriter<W: AsyncWrite + Unpin> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> WsWriter<W> {
    pub fn new(writer: W) -> WsWriter<W> {
        WsWriter { writer }
    }

    pub async fn send_text(&mut self, text: &str) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(text.len() + 10);
        write_frame(&mut buf, 0x1, text.as_bytes());
        self.writer.write_all(&buf).await
    }

    pub async fn send_pong(&mut self, payload: &[u8]) -> std::io::Result<()> {
        let mut buf = Vec::with_capacity(payload.len() + 10);
        write_frame(&mut buf, 0xA, payload);
        self.writer.write_all(&buf).await
    }

    pub async fn send_close(&mut self, code: u16, reason: &str) -> std::io::Result<()> {
        let mut body = code.to_be_bytes().to_vec();
        let reason_bytes = reason.as_bytes();
        let take = reason_bytes.len().min(123);
        body.extend_from_slice(&reason_bytes[..take]);
        let mut buf = Vec::with_capacity(body.len() + 10);
        write_frame(&mut buf, 0x8, &body);
        self.writer.write_all(&buf).await?;
        self.writer.flush().await
    }

    pub async fn shutdown(&mut self) {
        let _ = self.writer.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn masked(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![
            (if fin { 0x80 } else { 0 }) | opcode,
            0x80 | payload.len() as u8,
        ];
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(payload);
        out
    }
    #[tokio::test]
    async fn coalesced_fragments_do_not_wait_for_another_network_read() {
        let (mut peer, connection) = tokio::io::duplex(128);
        let mut bytes = masked(false, 1, b"hel");
        bytes.extend(masked(true, 0, b"lo"));
        peer.write_all(&bytes).await.unwrap();
        let mut reader = WsReader::new(connection);
        let event =
            tokio::time::timeout(std::time::Duration::from_millis(100), reader.next_event())
                .await
                .expect("buffered continuation must not stall")
                .unwrap_or_else(|_| panic!("valid fragments rejected"));
        assert!(matches!(event, WsEvent::Text(s) if s == "hello"));
    }
    #[test]
    fn fragmented_text_and_control_bounds() {
        let mut reader = WsReader::new(&b""[..]);
        reader.buffer = masked(false, 1, b"hel");
        assert!(reader
            .parse_one()
            .unwrap_or_else(|_| panic!("first fragment rejected"))
            .is_none());
        reader.buffer = masked(true, 0, b"lo");
        assert!(
            matches!(reader.parse_one().unwrap_or_else(|_| panic!("continuation rejected")), Some(WsEvent::Text(s)) if s == "hello")
        );
        reader.buffer = masked(false, 9, b"ping");
        assert!(reader.parse_one().is_err());
        reader.buffer = vec![0x81, 0xff];
        reader
            .buffer
            .extend_from_slice(&((MAX_MESSAGE_BYTES as u64) + 1).to_be_bytes());
        assert_eq!(reader.parse_one().err().unwrap().code, 1009);
        assert_eq!(
            reader.buffer.len(),
            10,
            "reject from header before reading the body"
        );
    }

    #[test]
    fn accept_key_matches_rfc6455_example() {
        // RFC 6455 §1.3 example handshake.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, 0x1, b"hello");
        assert_eq!(buf[0], 0x81);
        assert_eq!(buf[1], 5);
        assert_eq!(&buf[2..], b"hello");
    }
}
