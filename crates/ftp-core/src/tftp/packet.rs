//! TFTP packet codec (RFC 1350 §5 + RFC 2347/2348 option negotiation).
//!
//! Wire layout (all integers big-endian, strings NUL-terminated):
//! ```text
//! RRQ/WRQ : opcode(2) filename 0 mode 0 [option 0 value 0]...
//! DATA    : opcode(2)=3 block(2) payload...
//! ACK     : opcode(2)=4 block(2)
//! ERROR   : opcode(2)=5 code(2) message 0
//! OACK    : opcode(2)=6 [option 0 value 0]...
//! ```
//! Option names are matched case-insensitively per RFC 2347.

use crate::error::{Error, Result};

/// RFC 2348 blksize bounds.
pub const MIN_BLKSIZE: usize = 8;
pub const MAX_BLKSIZE: usize = 65464;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Rrq { filename: String, mode: String, options: Vec<(String, String)> },
    Wrq { filename: String, mode: String, options: Vec<(String, String)> },
    Data { block: u16, data: Vec<u8> },
    Ack { block: u16 },
    /// Option acknowledgement (RFC 2347/2348).
    Oack { options: Vec<(String, String)> },
    Error { code: u16, msg: String },
}

impl Packet {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Packet::Rrq { filename, mode, options } | Packet::Wrq { filename, mode, options } => {
                out.extend_from_slice(&self.opcode().to_be_bytes());
                push_z(&mut out, filename);
                push_z(&mut out, mode);
                for (k, v) in options {
                    push_z(&mut out, k);
                    push_z(&mut out, v);
                }
            }
            Packet::Data { block, data } => {
                out.extend_from_slice(&3u16.to_be_bytes());
                out.extend_from_slice(&block.to_be_bytes());
                out.extend_from_slice(data);
            }
            Packet::Ack { block } => {
                out.extend_from_slice(&4u16.to_be_bytes());
                out.extend_from_slice(&block.to_be_bytes());
            }
            Packet::Oack { options } => {
                out.extend_from_slice(&6u16.to_be_bytes());
                for (k, v) in options {
                    push_z(&mut out, k);
                    push_z(&mut out, v);
                }
            }
            Packet::Error { code, msg } => {
                out.extend_from_slice(&5u16.to_be_bytes());
                out.extend_from_slice(&code.to_be_bytes());
                push_z(&mut out, msg);
            }
        }
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Packet> {
        if buf.len() < 2 {
            return Err(Error::TftpProtocol("packet too short".into()));
        }
        let opcode = u16::from_be_bytes([buf[0], buf[1]]);
        match opcode {
            1 | 2 => {
                let (filename, pos) = read_z(buf, 2)?;
                let (mode, pos) = read_z(buf, pos).unwrap_or(("octet".into(), pos));
                let options = parse_options(buf, pos);
                if opcode == 1 {
                    Ok(Packet::Rrq { filename, mode, options })
                } else {
                    Ok(Packet::Wrq { filename, mode, options })
                }
            }
            3 => {
                if buf.len() < 4 {
                    return Err(Error::TftpProtocol("data packet too short".into()));
                }
                let block = u16::from_be_bytes([buf[2], buf[3]]);
                Ok(Packet::Data { block, data: buf[4..].to_vec() })
            }
            4 => {
                if buf.len() < 4 {
                    return Err(Error::TftpProtocol("ack packet too short".into()));
                }
                let block = u16::from_be_bytes([buf[2], buf[3]]);
                Ok(Packet::Ack { block })
            }
            5 => {
                if buf.len() < 4 {
                    return Err(Error::TftpProtocol("error packet too short".into()));
                }
                let code = u16::from_be_bytes([buf[2], buf[3]]);
                let msg_bytes = &buf[4..];
                let end = msg_bytes.iter().position(|&b| b == 0).unwrap_or(msg_bytes.len());
                Ok(Packet::Error {
                    code,
                    msg: String::from_utf8_lossy(&msg_bytes[..end]).into_owned(),
                })
            }
            6 => Ok(Packet::Oack { options: parse_options(buf, 2) }),
            other => Err(Error::TftpProtocol(format!("unknown opcode {other}"))),
        }
    }

    fn opcode(&self) -> u16 {
        match self {
            Packet::Rrq { .. } => 1,
            Packet::Wrq { .. } => 2,
            Packet::Data { .. } => 3,
            Packet::Ack { .. } => 4,
            Packet::Error { .. } => 5,
            Packet::Oack { .. } => 6,
        }
    }

    /// Convert a received ERROR packet into our error type.
    pub fn into_remote_error(self) -> Option<Error> {
        match self {
            Packet::Error { code, msg } => Some(Error::TftpRemote { code, msg }),
            _ => None,
        }
    }
}

/// Extract a negotiated blksize from an option list (case-insensitive name,
/// value clamped to the RFC 2348 range).
pub fn negotiated_blksize(options: &[(String, String)]) -> Option<usize> {
    options
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("blksize"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .map(|b| b.clamp(MIN_BLKSIZE, MAX_BLKSIZE))
}

fn push_z(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Read a NUL-terminated string starting at `from`; returns (string, next pos).
fn read_z(buf: &[u8], from: usize) -> Result<(String, usize)> {
    if from >= buf.len() {
        return Err(Error::TftpProtocol("unexpected end of packet".into()));
    }
    let rel = buf[from..]
        .iter()
        .position(|&b| b == 0)
        .ok_or_else(|| Error::TftpProtocol("missing string terminator".into()))?;
    Ok((
        String::from_utf8_lossy(&buf[from..from + rel]).into_owned(),
        from + rel + 1,
    ))
}

fn parse_options(buf: &[u8], mut pos: usize) -> Vec<(String, String)> {
    let mut opts = Vec::new();
    while pos < buf.len() {
        let (name, next) = match read_z(buf, pos) {
            Ok(v) => v,
            Err(_) => break,
        };
        let (value, after) = match read_z(buf, next) {
            Ok(v) => v,
            Err(_) => break,
        };
        opts.push((name, value));
        pos = after;
    }
    opts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_variants() {
        let cases = vec![
            Packet::Rrq { filename: "a.bin".into(), mode: "octet".into(), options: vec![] },
            Packet::Wrq {
                filename: "b.bin".into(),
                mode: "octet".into(),
                options: vec![("blksize".into(), "8192".into()), ("tsize".into(), "0".into())],
            },
            Packet::Data { block: 7, data: vec![1, 2, 3] },
            Packet::Ack { block: 65535 },
            Packet::Oack { options: vec![("blksize".into(), "8192".into())] },
            Packet::Oack { options: vec![] },
            Packet::Error { code: 1, msg: "File not found".into() },
        ];
        for p in cases {
            assert_eq!(Packet::decode(&p.encode()).unwrap(), p);
        }
    }

    #[test]
    fn decodes_legacy_rrq_without_options() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u16.to_be_bytes());
        raw.extend_from_slice(b"file.txt\0octet\0");
        match Packet::decode(&raw).unwrap() {
            Packet::Rrq { filename, options, .. } => {
                assert_eq!(filename, "file.txt");
                assert!(options.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn blksize_negotiation_clamps() {
        let opts = vec![("BLKSIZE".to_string(), "99999999".to_string())];
        assert_eq!(negotiated_blksize(&opts), Some(MAX_BLKSIZE));
        let opts = vec![("blksize".to_string(), "1".to_string())];
        assert_eq!(negotiated_blksize(&opts), Some(MIN_BLKSIZE));
        let opts = vec![("tsize".to_string(), "0".to_string())];
        assert_eq!(negotiated_blksize(&opts), None);
    }
}
