//! 측정기 두 끝이 주고받는 틀.
//!
//! 제어(양방향 스트림 하나): `[u8 종류][u32 길이 BE][내용]`.
//! 프레임(프레임마다 단방향 스트림): `'F'` + `FrameHead` + 압축 자료. 원본 한 장: `'S'` + `u64 cap_id` + PNG.

use anyhow::{bail, Context, Result};
use iroh::endpoint::{RecvStream, SendStream};

pub const ALPN: &[u8] = b"kasa/screen-probe/1";

pub mod ctl {
    pub const CONFIG: u8 = b'C';
    pub const INFO: u8 = b'I';
    pub const PING: u8 = b'P';
    pub const PONG: u8 = b'p';
    pub const KEYFRAME: u8 = b'K';
    pub const DUMP: u8 = b'D';
    pub const QUIT: u8 = b'Q';
}

pub async fn write_msg(send: &mut SendStream, kind: u8, body: &[u8]) -> Result<()> {
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(kind);
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(body);
    send.write_all(&buf).await?;
    Ok(())
}

pub async fn read_msg(recv: &mut RecvStream) -> Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    recv.read_exact(&mut head).await?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > 1 << 20 {
        bail!("제어 메시지가 너무 크다: {len}");
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await?;
    Ok((head[0], body))
}

/// 시각은 모두 보낸 쪽 호스트 시계(초). 받는 쪽은 시계 차이를 빼서 쓴다.
#[derive(Debug, Clone)]
pub struct FrameHead {
    pub seq: u32,
    pub cap_id: u64,
    pub key: bool,
    /// 화면에 그려진 시각(SCK 프레임의 PTS).
    pub t_cap: f64,
    /// SCK 가 프레임을 넘겨준 시각.
    pub t_cb: f64,
    /// 인코더가 압축을 끝낸 시각.
    pub t_enc: f64,
    pub width: u16,
    pub height: u16,
    /// 키프레임일 때만 — VPS/SPS/PPS(HEVC) 또는 SPS/PPS(H.264).
    pub param_sets: Vec<Vec<u8>>,
}

impl FrameHead {
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.push(b'F');
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.cap_id.to_be_bytes());
        out.push(u8::from(self.key));
        for t in [self.t_cap, self.t_cb, self.t_enc] {
            out.extend_from_slice(&t.to_be_bytes());
        }
        out.extend_from_slice(&self.width.to_be_bytes());
        out.extend_from_slice(&self.height.to_be_bytes());
        out.push(self.param_sets.len() as u8);
        for ps in &self.param_sets {
            out.extend_from_slice(&(ps.len() as u16).to_be_bytes());
            out.extend_from_slice(ps);
        }
    }

    /// `buf` 는 `'F'` 다음부터. 머리와 나머지(압축 자료)의 시작 위치를 돌려준다.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        let mut r = Reader { buf, at: 0 };
        let seq = u32::from_be_bytes(r.take::<4>()?);
        let cap_id = u64::from_be_bytes(r.take::<8>()?);
        let key = r.take::<1>()?[0] != 0;
        let t_cap = f64::from_be_bytes(r.take::<8>()?);
        let t_cb = f64::from_be_bytes(r.take::<8>()?);
        let t_enc = f64::from_be_bytes(r.take::<8>()?);
        let width = u16::from_be_bytes(r.take::<2>()?);
        let height = u16::from_be_bytes(r.take::<2>()?);
        let n = r.take::<1>()?[0] as usize;
        let mut param_sets = Vec::with_capacity(n);
        for _ in 0..n {
            let len = u16::from_be_bytes(r.take::<2>()?) as usize;
            param_sets.push(r.slice(len)?.to_vec());
        }
        let head = Self { seq, cap_id, key, t_cap, t_cb, t_enc, width, height, param_sets };
        Ok((head, r.at))
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.slice(N)?.try_into().expect("길이를 맞췄다"))
    }

    fn slice(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.buf.get(self.at..self.at + n).context("프레임 머리가 잘렸다")?;
        self.at += n;
        Ok(s)
    }
}

pub fn f64_pair(a: f64, b: f64) -> Vec<u8> {
    let mut v = a.to_be_bytes().to_vec();
    v.extend_from_slice(&b.to_be_bytes());
    v
}

pub fn read_f64_pair(b: &[u8]) -> Result<(f64, f64)> {
    let a: [u8; 8] = b.get(0..8).context("짧다")?.try_into()?;
    let c: [u8; 8] = b.get(8..16).context("짧다")?.try_into()?;
    Ok((f64::from_be_bytes(a), f64::from_be_bytes(c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_round_trip() {
        let h = FrameHead {
            seq: 7,
            cap_id: 3,
            key: true,
            t_cap: 1.5,
            t_cb: 1.501,
            t_enc: 1.504,
            width: 3024,
            height: 1964,
            param_sets: vec![vec![1, 2, 3], vec![4]],
        };
        let mut buf = Vec::new();
        h.encode(&mut buf);
        buf.extend_from_slice(b"payload");
        let (back, at) = FrameHead::decode(&buf[1..]).unwrap();
        assert_eq!(back.seq, 7);
        assert_eq!(back.cap_id, 3);
        assert!(back.key);
        assert_eq!(back.param_sets, h.param_sets);
        assert_eq!(&buf[1 + at..], b"payload");
    }
}
