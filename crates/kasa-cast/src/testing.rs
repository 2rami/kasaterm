//! 시험용 가짜 보내기 — 이 크레이트와 웹소켓 쪽(`kasa-remote`) 시험이 함께 쓴다.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::{now, Capture, Config, Denied, Frame, Input, Session};

#[derive(Default)]
pub struct Counts {
    pub started: AtomicUsize,
    pub stopped: AtomicUsize,
    pub keyframes: AtomicUsize,
    pub prompts: AtomicUsize,
    pub input_prompts: AtomicUsize,
    pub inputs: Mutex<Vec<(u32, Input)>>,
    pub cursor_hidden: Mutex<Vec<bool>>,
}

/// 5ms 마다 장을 낸다. 첫 장·청한 다음 장이 키프레임이다.
pub struct FakeCapture {
    pub counts: Arc<Counts>,
    pub deny: Option<Denied>,
    pub deny_input: Option<Denied>,
}

struct FakeSession {
    counts: Arc<Counts>,
    stop: Arc<AtomicBool>,
    want_key: Arc<AtomicBool>,
}

impl Capture for FakeCapture {
    fn start(&self, config: &Config, out: Box<dyn Fn(Frame) + Send + Sync>) -> Result<Box<dyn Session>, Denied> {
        if let Some(d) = self.deny {
            return Err(d);
        }
        assert_eq!(config.fps, 30, "보는 쪽 설정이 그대로 온다");
        self.counts.started.fetch_add(1, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        let want_key = Arc::new(AtomicBool::new(true));
        let (s, k) = (stop.clone(), want_key.clone());
        std::thread::spawn(move || {
            let mut seq = 0u32;
            while !s.load(Ordering::Relaxed) {
                seq += 1;
                let key = k.swap(false, Ordering::Relaxed);
                out(Frame {
                    seq,
                    cap_id: u64::from(seq),
                    key,
                    t_cap: 1.0,
                    t_enc: 1.001,
                    width: 64,
                    height: 32,
                    param_sets: if key { vec![vec![1, 2], vec![3]] } else { Vec::new() },
                    data: vec![seq as u8; 100],
                });
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Ok(Box::new(FakeSession { counts: self.counts.clone(), stop, want_key }))
    }

    fn request_permission(&self) {
        self.counts.prompts.fetch_add(1, Ordering::Relaxed);
    }

    fn input(&self, display: u32, input: &Input) -> Result<(), Denied> {
        if let Some(d) = self.deny_input {
            return Err(d);
        }
        self.counts.inputs.lock().unwrap().push((display, input.clone()));
        Ok(())
    }

    fn request_input_permission(&self) {
        self.counts.input_prompts.fetch_add(1, Ordering::Relaxed);
    }

    fn now(&self) -> f64 {
        now() + 100.0
    }
}

impl Session for FakeSession {
    fn keyframe(&self) {
        self.counts.keyframes.fetch_add(1, Ordering::Relaxed);
        self.want_key.store(true, Ordering::Relaxed);
    }

    fn info(&self) -> Value {
        json!({ "width": 64, "height": 32 })
    }

    fn display(&self) -> u32 {
        7
    }

    fn set_cursor_hidden(&self, hidden: bool) {
        self.counts.cursor_hidden.lock().unwrap().push(hidden);
    }
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.counts.stopped.fetch_add(1, Ordering::Relaxed);
    }
}

