use std::io::{Error, ErrorKind, Result};

pub(crate) const CHUNK: usize = 65_536;
pub(crate) const ZLIB_V1: &str = "zlib-v1";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Codec {
    #[default]
    Raw,
    Zlib,
}

impl Codec {
    pub(crate) fn negotiated(name: Option<&str>) -> Self {
        if name == Some(ZLIB_V1) {
            Self::Zlib
        } else {
            Self::Raw
        }
    }

    pub(crate) fn name(self) -> Option<&'static str> {
        (self == Self::Zlib).then_some(ZLIB_V1)
    }

    pub(crate) fn encode(self, bytes: &[u8]) -> Vec<u8> {
        assert!(bytes.len() <= CHUNK);
        if self == Self::Raw {
            return bytes.to_vec();
        }
        // Tiny updates and already-compressed images should not pay for expansion.
        if bytes.len() >= 256 {
            let packed = miniz_oxide::deflate::compress_to_vec_zlib(bytes, 1);
            if packed.len() + 32 < bytes.len() {
                let mut frame = Vec::with_capacity(packed.len() + 1);
                frame.push(1);
                frame.extend(packed);
                return frame;
            }
        }
        let mut frame = Vec::with_capacity(bytes.len() + 1);
        frame.push(0);
        frame.extend_from_slice(bytes);
        frame
    }

    pub(crate) fn decode(self, frame: &[u8]) -> Result<Vec<u8>> {
        if self == Self::Raw {
            return Ok(frame.to_vec());
        }
        let invalid = || Error::new(ErrorKind::InvalidData, "invalid terminal compression frame");
        if frame.len() > CHUNK + 1 {
            return Err(invalid());
        }
        match frame.split_first() {
            Some((0, bytes)) => Ok(bytes.to_vec()),
            // A corrupt or oversized update must reconnect, never enter the VT parser.
            Some((1, bytes)) => {
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(bytes, CHUNK)
                    .map_err(|_| invalid())
            }
            _ => Err(invalid()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Codec, CHUNK, ZLIB_V1};

    #[test]
    fn old_peers_keep_exact_binary_bytes() {
        for name in [None, Some("unknown")] {
            let codec = Codec::negotiated(name);
            let raw = b"\x00\x01\x1b[?2026h\xff";
            assert_eq!(codec.encode(raw), raw);
            assert_eq!(codec.decode(raw).unwrap(), raw);
        }
        assert_eq!(Codec::negotiated(Some(ZLIB_V1)).name(), Some(ZLIB_V1));
    }

    #[test]
    fn ansi_hangul_and_graphics_survive_arbitrary_chunk_boundaries() {
        let mut raw = "\x1b[?2026h\x1b[38;2;23;45;67m한글 표 │ 결과\r\n"
            .repeat(4000)
            .into_bytes();
        raw.extend(b"\x1b_Ga=T,f=100;image-data\x1b\\\x1b[?2026l");
        raw.extend(0..=255);
        for size in [1, 7, 511, CHUNK] {
            let restored: Vec<u8> = raw
                .chunks(size)
                .flat_map(|part| Codec::Zlib.decode(&Codec::Zlib.encode(part)).unwrap())
                .collect();
            assert_eq!(restored, raw);
        }
        assert!(Codec::Zlib.encode(&raw[..CHUNK]).len() < CHUNK / 4);
    }

    #[test]
    fn incompressible_chunks_remain_bounded() {
        let mut seed = 12345u32;
        let raw: Vec<_> = (0..CHUNK)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let frame = Codec::Zlib.encode(&raw);
        assert_eq!(frame.len(), CHUNK + 1);
        assert_eq!(frame[0], 0);
        assert_eq!(Codec::Zlib.decode(&frame).unwrap(), raw);
    }

    #[test]
    fn corrupt_truncated_and_expanding_frames_are_rejected() {
        for frame in [vec![], vec![2], vec![1, 0], vec![0; CHUNK + 2]] {
            assert!(Codec::Zlib.decode(&frame).is_err());
        }
        let mut packed = Codec::Zlib.encode(&vec![b'x'; CHUNK]);
        assert_eq!(packed[0], 1);
        assert!(Codec::Zlib.decode(&packed[..packed.len() - 1]).is_err());
        *packed.last_mut().unwrap() ^= 1;
        assert!(Codec::Zlib.decode(&packed).is_err());
        let mut bomb = vec![1];
        bomb.extend(miniz_oxide::deflate::compress_to_vec_zlib(
            &vec![0; CHUNK * 8],
            1,
        ));
        assert!(Codec::Zlib.decode(&bomb).is_err());
    }
}
