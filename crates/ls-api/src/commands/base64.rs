//! RFC 4648 §4 standard base64 (with padding) — hand-rolled so the wire
//! layer keeps ls-api dependency-free (manifest is outside T10 ownership).
//! Decoder is strict: any non-alphabet byte, misplaced '=', or bad final
//! quantum is an error, never silently dropped.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn deval(b: u8) -> Option<u32> {
    match b {
        b'A'..=b'Z' => Some((b - b'A') as u32),
        b'a'..=b'z' => Some((b - b'a' + 26) as u32),
        b'0'..=b'9' => Some((b - b'0' + 52) as u32),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    let bytes = text.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err("base64 length must be a multiple of 4 (padding required)".to_string());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (qi, q) in bytes.chunks_exact(4).enumerate() {
        let last = qi == bytes.len() / 4 - 1;
        let pad = q.iter().filter(|&&c| c == b'=').count();
        if pad > 0 {
            if !last || pad > 2 || !q[4 - pad..].iter().all(|&c| c == b'=') {
                return Err("'=' padding only allowed at the very end (1 or 2 chars)".to_string());
            }
        }
        let mut n: u32 = 0;
        for (i, &c) in q.iter().enumerate() {
            let v = if c == b'=' && last && i >= 4 - pad {
                0
            } else {
                deval(c).ok_or_else(|| format!("invalid base64 byte 0x{c:02x}"))?
            };
            n = (n << 6) | v;
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_known_answer_vectors() {
        // RFC 4648 §10 test vectors.
        let kat: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for (raw, enc) in kat {
            assert_eq!(encode(raw), *enc);
            assert_eq!(decode(enc).unwrap(), raw.to_vec());
        }
    }

    #[test]
    fn arbitrary_byte_roundtrip_including_non_utf8() {
        let mut corpus: Vec<Vec<u8>> = vec![
            (0u8..=255).collect(),
            vec![0xff, 0xfe, 0xfd, 0xfc],
            vec![0x00; 17],
            b"hello\x00world".to_vec(),
        ];
        // edge lengths around the 3-byte quantum
        for n in 1..=8usize {
            corpus.push(vec![0xabu8; n]);
        }
        for raw in corpus {
            let enc = encode(&raw);
            assert_eq!(decode(&enc).unwrap(), raw, "roundtrip {raw:?}");
            // encoded form must be valid ASCII text
            assert!(enc.is_ascii());
        }
    }

    #[test]
    fn decoder_is_strict() {
        assert!(decode("Zg=").is_err()); // bad length
        assert!(decode("Z===").is_err()); // too much padding
        assert!(decode("=Zg=").is_err()); // padding not at end
        assert!(decode("Zg==Zg==").is_err()); // mid-stream padding
        assert!(decode("Zm9!").is_err()); // outside alphabet
        assert!(decode("Zm9\n").is_err()); // whitespace not skipped
        // canonical round: no trailing garbage accepted
        assert_eq!(decode("Zg==").unwrap(), b"f");
    }
}
