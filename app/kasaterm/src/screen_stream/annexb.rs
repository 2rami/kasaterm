//! Annex-B(시작 부호 `00 00 01`) ↔ 화면 스트림 장 꼴. Windows Media Foundation 인코더·디코더는 Annex-B 를 쓰고, 화면 스트림 장은
//! 4바이트 길이 머리 NAL(AVCC/HVCC)과 따로 떼어 낸 파라미터 셋(VPS·SPS·PPS)을 싣는다(`kasa_remote::screen_stream::Frame`).

#[cfg_attr(not(windows), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nal {
    Hevc,
    H264,
}

/// 파라미터 셋이 아닌 NAL 은 4바이트 길이 머리를 붙여 이어 붙이고, 파라미터 셋은 따로 돌려준다. 키프레임(IDR·CRA)이 있으면 참.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn split(stream: &[u8], kind: Nal) -> (Vec<Vec<u8>>, Vec<u8>, bool) {
    let (mut sets, mut data, mut key) = (Vec::new(), Vec::with_capacity(stream.len() + 16), false);
    for nal in units(stream) {
        let Some(&first) = nal.first() else { continue };
        let (is_set, is_key) = match kind {
            // HEVC 머리 6비트: VPS 32·SPS 33·PPS 34, IRAP 16–21.
            Nal::Hevc => {
                let t = (first >> 1) & 0x3f;
                ((32..=34).contains(&t), (16..=21).contains(&t))
            }
            // H.264 머리 5비트: SPS 7·PPS 8, IDR 5.
            Nal::H264 => {
                let t = first & 0x1f;
                (t == 7 || t == 8, t == 5)
            }
        };
        key |= is_key;
        if is_set {
            sets.push(nal.to_vec());
        } else {
            data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            data.extend_from_slice(nal);
        }
    }
    (sets, data, key)
}

/// `split` 의 반대 — 파라미터 셋을 앞에 두고 길이 머리를 시작 부호로 바꾼다. 길이가 넘치는 끝 조각은 버린다.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn join(sets: &[Vec<u8>], data: &[u8]) -> Vec<u8> {
    const START: [u8; 4] = [0, 0, 0, 1];
    let mut out = Vec::with_capacity(data.len() + sets.iter().map(|s| s.len() + 4).sum::<usize>());
    for set in sets {
        out.extend_from_slice(&START);
        out.extend_from_slice(set);
    }
    let mut i = 0;
    while let Some(head) = data.get(i..i + 4) {
        let n = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let Some(nal) = data.get(i + 4..i + 4 + n) else { break };
        out.extend_from_slice(&START);
        out.extend_from_slice(nal);
        i += 4 + n;
    }
    out
}

/// 시작 부호(3·4바이트)로 가른 NAL 들 — 끝에 붙은 0(trailing zero)은 다음 시작 부호의 일부라 뗀다.
fn units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &s)| {
            let mut end = starts.get(n + 1).map_or(stream.len(), |&next| next - 3);
            while end > s && stream[end - 1] == 0 {
                end -= 1;
            }
            &stream[s..end]
        })
        .filter(|u| !u.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hevc_keyframe_splits_parameter_sets_and_length_prefixes_the_rest() {
        // VPS(32) SPS(33) PPS(34) IDR_W_RADL(19) — 4·3바이트 시작 부호가 섞여 온다.
        let s = [
            0, 0, 0, 1, 0x40, 1, 0xaa, //
            0, 0, 1, 0x42, 1, 0xbb, //
            0, 0, 0, 1, 0x44, 1, 0xcc, //
            0, 0, 0, 1, 0x26, 1, 0xdd, 0xee, 0, 0,
        ];
        let (sets, data, key) = split(&s, Nal::Hevc);
        assert!(key);
        assert_eq!(sets, vec![vec![0x40, 1, 0xaa], vec![0x42, 1, 0xbb], vec![0x44, 1, 0xcc]]);
        assert_eq!(data, vec![0, 0, 0, 4, 0x26, 1, 0xdd, 0xee], "뒤에 붙은 0 은 NAL 이 아니다");
    }

    #[test]
    fn hevc_p_frame_is_not_a_key_and_h264_types_are_read_from_five_bits() {
        let (sets, data, key) = split(&[0, 0, 1, 0x02, 1, 9], Nal::Hevc);
        assert!(!key && sets.is_empty());
        assert_eq!(data, vec![0, 0, 0, 3, 0x02, 1, 9]);
        let (sets, data, key) = split(&[0, 0, 0, 1, 0x67, 7, 0, 0, 1, 0x68, 8, 0, 0, 1, 0x65, 5], Nal::H264);
        assert!(key);
        assert_eq!(sets, vec![vec![0x67, 7], vec![0x68, 8]]);
        assert_eq!(data, vec![0, 0, 0, 2, 0x65, 5]);
        assert_eq!(split(&[], Nal::H264), (Vec::new(), Vec::new(), false));
    }

    #[test]
    fn join_undoes_split_and_drops_a_truncated_tail() {
        let sets = vec![vec![0x67, 7, 1], vec![0x68, 8]];
        let data = vec![0, 0, 0, 3, 0x65, 5, 9, 0, 0, 0, 2, 0x41, 1];
        let stream = join(&sets, &data);
        assert_eq!(&stream[..4], &[0, 0, 0, 1]);
        assert_eq!(split(&stream, Nal::H264), (sets, data.clone(), true));
        let mut cut = data;
        cut.extend_from_slice(&[0, 0, 0, 9, 0x41]);
        assert_eq!(split(&join(&[], &cut), Nal::H264).1, cut[..13].to_vec(), "길이가 넘치는 끝 조각은 싣지 않는다");
    }
}
