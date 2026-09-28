//! 누가 더 새것인가를 **시계 없이** 가른다. 기기마다 시계가 어긋나고, `cp -p`·압축
//! 풀기는 옛 mtime 을 그대로 들고 오므로 「mtime 이 큰 쪽」은 멀쩡한 새 파일을 옛것에
//! 지게 만든다. 그래서 파일마다 기기별 번호표(version vector)를 단다.
//!
//! 번호는 `max(이전+1, 지금 ms)` 로 올린다 — 제 기기의 옛 번호하고만 비교되므로 기기끼리
//! 시계가 달라도 상관없고, 색인을 잃어 0 에서 다시 세도 옛 번호보다 뒤로 가지 않는다.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Vv = BTreeMap<String, u64>;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct Entry {
    /// NFC, `/` 로 이은 표시 경로. 같은 파일인지는 `path::key` 로 가른다.
    pub path: String,
    pub vv: Vv,
    #[serde(default)]
    pub sha: String,
    #[serde(default)]
    pub size: u64,
    /// 만든 기기의 mtime — 받는 쪽이 그대로 입혀 둔다(보기용, 판정에는 안 쓴다).
    #[serde(default)]
    pub mtime_ms: u64,
    #[serde(default)]
    pub deleted: bool,
    /// 이 내용을 만든 기기 이름. 충돌 사본 이름과 폰 표시에 쓴다.
    #[serde(default)]
    pub origin: String,
    /// 이 기기 색인에서 마지막으로 바뀐 순번 — 다른 기기가 여기부터 달라고 한다.
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Order {
    Equal,
    Before,
    After,
    Concurrent,
}

/// `a` 를 `b` 에 대어 본다. `After` 면 `a` 가 `b` 를 다 알고 더 안다.
pub fn compare(a: &Vv, b: &Vv) -> Order {
    let (mut less, mut greater) = (false, false);
    for k in a.keys().chain(b.keys()) {
        let (x, y) = (a.get(k).copied().unwrap_or(0), b.get(k).copied().unwrap_or(0));
        less |= x < y;
        greater |= x > y;
    }
    match (less, greater) {
        (false, false) => Order::Equal,
        (true, false) => Order::Before,
        (false, true) => Order::After,
        (true, true) => Order::Concurrent,
    }
}

pub fn merge(a: &Vv, b: &Vv) -> Vv {
    let mut out = a.clone();
    for (k, v) in b {
        let slot = out.entry(k.clone()).or_insert(0);
        *slot = (*slot).max(*v);
    }
    out
}

pub fn bump(vv: &Vv, me: &str, now_ms: u64) -> Vv {
    let mut out = vv.clone();
    let next = out.get(me).map_or(now_ms, |v| (v + 1).max(now_ms));
    out.insert(me.to_string(), next);
    out
}

/// 동시에 갈린 두 판 중 이긴 쪽 — 어느 기기에서 재도 같은 답이 나와야 한다.
fn wins(a: &Entry, b: &Entry) -> bool {
    fn rank(e: &Entry) -> Vec<(u64, &str)> {
        let mut r: Vec<(u64, &str)> = e.vv.iter().map(|(k, v)| (*v, k.as_str())).collect();
        r.sort_unstable_by(|x, y| y.cmp(x));
        r
    }
    (rank(a), &a.sha) > (rank(b), &b.sha)
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Decision {
    Skip,
    /// 파일은 그대로 두고 색인만 이 판으로.
    Adopt(Entry),
    /// 상대 내용을 받아 이 경로에 둔다.
    Fetch(Entry),
    /// 이 기기 사본을 휴지통으로.
    Trash(Entry),
    /// 두 기기가 같은 파일을 따로 고쳤다. 경로에는 이긴 내용, 진 내용은
    /// `path (만든 기기).ext` 사본으로 남긴다 — 어느 쪽 내용도 잃지 않는다.
    KeepBoth { entry: Entry, loser: Entry, local_wins: bool },
}

pub fn decide(local: Option<&Entry>, remote: &Entry) -> Decision {
    use Decision::*;
    let Some(local) = local else {
        return if remote.deleted { Adopt(remote.clone()) } else { Fetch(remote.clone()) };
    };
    match compare(&remote.vv, &local.vv) {
        Order::Equal | Order::Before => Skip,
        Order::After => match (local.deleted, remote.deleted) {
            (true, true) => Adopt(remote.clone()),
            (false, true) => Trash(remote.clone()),
            (true, false) => Fetch(remote.clone()),
            (false, false) if local.sha == remote.sha => Adopt(remote.clone()),
            (false, false) => Fetch(remote.clone()),
        },
        Order::Concurrent => {
            let vv = merge(&local.vv, &remote.vv);
            let remote_wins = wins(remote, local);
            let winner = if remote_wins { remote } else { local };
            let merged = Entry { vv, ..winner.clone() };
            match (local.deleted, remote.deleted) {
                (true, true) => Adopt(merged),
                // 지움과 고침이 엇갈리면 고친 쪽이 산다.
                (false, true) => Adopt(Entry { vv: merged.vv, ..local.clone() }),
                (true, false) => Fetch(Entry { vv: merged.vv, ..remote.clone() }),
                (false, false) if local.sha == remote.sha => Adopt(merged),
                (false, false) => KeepBoth {
                    entry: merged,
                    loser: if remote_wins { local.clone() } else { remote.clone() },
                    local_wins: !remote_wins,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::share::path::{conflict_name, key};

    fn vv(pairs: &[(&str, u64)]) -> Vv {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn entry(path: &str, v: &[(&str, u64)], sha: &str) -> Entry {
        Entry { path: path.into(), vv: vv(v), sha: sha.into(), origin: v[0].0.into(), ..Default::default() }
    }

    #[test]
    fn compare_orders() {
        assert_eq!(compare(&vv(&[("a", 1)]), &vv(&[("a", 1)])), Order::Equal);
        assert_eq!(compare(&vv(&[("a", 2)]), &vv(&[("a", 1)])), Order::After);
        assert_eq!(compare(&vv(&[("a", 1)]), &vv(&[("a", 1), ("b", 1)])), Order::Before);
        assert_eq!(compare(&vv(&[("a", 2)]), &vv(&[("a", 1), ("b", 1)])), Order::Concurrent);
    }

    #[test]
    fn bump_survives_lost_index_and_skewed_clock() {
        let old = vv(&[("a", 9_000)]);
        assert_eq!(bump(&old, "a", 10)["a"], 9_001);
        assert_eq!(bump(&Vv::new(), "a", 12_000)["a"], 12_000);
        assert_eq!(compare(&bump(&Vv::new(), "a", 12_000), &old), Order::After);
    }

    #[test]
    fn decide_branches() {
        let base = entry("x.png", &[("a", 1)], "s1");
        let newer = entry("x.png", &[("a", 2)], "s2");
        assert_eq!(decide(None, &base), Decision::Fetch(base.clone()));
        let tomb = Entry { deleted: true, ..entry("x.png", &[("a", 2)], "s1") };
        assert_eq!(decide(None, &tomb), Decision::Adopt(tomb.clone()));
        assert_eq!(decide(Some(&newer), &base), Decision::Skip);
        assert_eq!(decide(Some(&base), &base), Decision::Skip);
        assert_eq!(decide(Some(&base), &newer), Decision::Fetch(newer.clone()));
        assert_eq!(decide(Some(&base), &tomb), Decision::Trash(tomb.clone()));
        let touched = Entry { sha: "s1".into(), ..newer.clone() };
        assert_eq!(decide(Some(&base), &touched), Decision::Adopt(touched.clone()));
    }

    #[test]
    fn first_sync_of_identical_files_only_merges() {
        let a = entry("x.png", &[("a", 5)], "same");
        let b = entry("x.png", &[("b", 7)], "same");
        let Decision::Adopt(e) = decide(Some(&a), &b) else { panic!() };
        assert_eq!(e.vv, vv(&[("a", 5), ("b", 7)]));
    }

    #[test]
    fn edit_beats_concurrent_delete_on_both_sides() {
        let edit = entry("x.png", &[("a", 1), ("b", 5)], "s2");
        let del = Entry { deleted: true, ..entry("x.png", &[("a", 3)], "s1") };
        let Decision::Adopt(kept) = decide(Some(&edit), &del) else { panic!() };
        assert!(!kept.deleted && kept.sha == "s2");
        let Decision::Fetch(got) = decide(Some(&del), &edit) else { panic!() };
        assert_eq!(got.sha, "s2");
        assert_eq!(kept.vv, got.vv);
    }

    #[test]
    fn concurrent_edits_agree_on_winner_from_either_side() {
        let a = entry("x.png", &[("a", 9)], "sa");
        let b = entry("x.png", &[("b", 4)], "sb");
        let Decision::KeepBoth { entry: ea, loser: la, local_wins: wa } = decide(Some(&a), &b) else { panic!() };
        let Decision::KeepBoth { entry: eb, loser: lb, local_wins: wb } = decide(Some(&b), &a) else { panic!() };
        assert_eq!(ea, eb);
        assert_eq!(la, lb);
        assert!(wa && !wb);
        assert_eq!(ea.sha, "sa");
    }

    /// 기기 넷을 메모리에 두고 실제 흐름(훑기·받기)을 고정점까지 돌린다.
    #[derive(Default, Clone)]
    struct Box_ {
        id: String,
        clock: u64,
        index: BTreeMap<String, Entry>,
        disk: BTreeMap<String, (String, String)>,
    }

    impl Box_ {
        fn new(id: &str, clock: u64) -> Self {
            Self { id: id.into(), clock, ..Default::default() }
        }
        fn now(&mut self) -> u64 {
            self.clock += 1;
            self.clock
        }
        fn write(&mut self, path: &str, sha: &str) {
            self.disk.insert(key(path), (path.into(), sha.into()));
        }
        fn record(&mut self, e: Entry) {
            self.index.insert(key(&e.path), e);
        }
        fn scan(&mut self) -> bool {
            let mut changed = false;
            for (k, (path, sha)) in self.disk.clone() {
                let old = self.index.get(&k).cloned();
                if old.as_ref().is_some_and(|e| !e.deleted && &e.sha == &sha) {
                    continue;
                }
                let now = self.now();
                let vv = bump(&old.map(|e| e.vv).unwrap_or_default(), &self.id, now);
                let origin = self.id.clone();
                self.record(Entry { path, vv, sha, origin, ..Default::default() });
                changed = true;
            }
            for (k, e) in self.index.clone() {
                if !e.deleted && !self.disk.contains_key(&k) {
                    let now = self.now();
                    let vv = bump(&e.vv, &self.id, now);
                    self.record(Entry { deleted: true, vv, origin: self.id.clone(), ..e });
                    changed = true;
                }
            }
            changed
        }
        fn content(&self, e: &Entry) -> Option<String> {
            self.disk.get(&key(&e.path)).map(|(_, s)| s.clone()).filter(|s| s == &e.sha)
        }
        fn pull(&mut self, from: &Box_) -> bool {
            let mut changed = false;
            for remote in from.index.values() {
                let k = key(&remote.path);
                match decide(self.index.get(&k), remote) {
                    Decision::Skip => continue,
                    Decision::Adopt(e) => self.record(e),
                    Decision::Fetch(e) => {
                        let Some(sha) = from.content(&e) else { continue };
                        self.write(&e.path, &sha);
                        self.record(e);
                    }
                    Decision::Trash(e) => {
                        self.disk.remove(&k);
                        self.record(e);
                    }
                    Decision::KeepBoth { entry, loser, local_wins } => {
                        let taken = |c: &str| self.disk.contains_key(c) || self.index.contains_key(c);
                        let copy = conflict_name(&loser.path, &loser.origin, taken);
                        if local_wins {
                            let Some(sha) = from.content(&loser) else { continue };
                            self.write(&copy, &sha);
                        } else {
                            let Some(sha) = from.content(&entry) else { continue };
                            let (_, mine) = self.disk.remove(&k).unwrap();
                            self.write(&copy, &mine);
                            self.write(&entry.path, &sha);
                        }
                        self.record(entry);
                    }
                }
                changed = true;
            }
            changed
        }
    }

    fn run(boxes: &mut [Box_], links: &[(usize, usize)]) {
        for round in 0.. {
            assert!(round < 50, "never settled");
            let mut changed = false;
            for b in boxes.iter_mut() {
                changed |= b.scan();
            }
            for &(x, y) in links {
                let (from_y, from_x) = (boxes[y].clone(), boxes[x].clone());
                changed |= boxes[x].pull(&from_y);
                changed |= boxes[y].pull(&from_x);
            }
            if !changed {
                return;
            }
        }
    }

    /// 대소문자만 다른 이름은 같은 파일이다 — 기기마다 처음 본 표기를 그대로 둔다.
    fn files(b: &Box_) -> BTreeMap<String, String> {
        b.disk.iter().map(|(k, (_, s))| (k.clone(), s.clone())).collect()
    }

    #[test]
    fn four_machines_converge_without_losing_edits() {
        // 맥북 A·B 는 서로 길이 없고 맥미니 M 을 거친다. 윈도우 W 는 나중에 켜진다.
        // 시계는 제각각이다 — B 는 한참 뒤처져 있다.
        let mut m = [Box_::new("a", 1_000_000), Box_::new("m", 50), Box_::new("b", 3), Box_::new("w", 7_000)];
        let (a, mm, b, w) = (0, 1, 2, 3);
        let net = [(a, mm), (b, mm)];
        m[a].write("시안/x.png", "x1");
        m[a].write("시안/y.png", "y1");
        m[a].write("시안/z.png", "z1");
        m[b].write("시안/same.png", "s");
        m[a].write("시안/SAME.png", "s");
        run(&mut m, &[(a, mm), (b, mm), (w, mm)]);
        assert!(m.iter().all(|x| files(x) == files(&m[0])));
        assert!(!files(&m[0]).keys().any(|p| p.contains('(')), "identical files must not fork");

        // W 가 꺼진 사이 A·B 가 x 를 따로 고치고, A 는 y 를 지우고, B 는 A 가 지운 z 를 고친다.
        m[a].write("시안/x.png", "xa");
        m[b].write("시안/x.png", "xb");
        m[a].disk.remove(&key("시안/y.png"));
        m[a].disk.remove(&key("시안/z.png"));
        m[b].write("시안/z.png", "zb");
        run(&mut m, &net);
        run(&mut m, &[(a, mm), (b, mm), (w, mm)]);

        let all = files(&m[0]);
        assert!(m.iter().all(|x| files(x) == all), "{:?}", m.iter().map(files).collect::<Vec<_>>());
        let shas: Vec<&str> = all.values().map(String::as_str).collect();
        assert!(shas.contains(&"xa") && shas.contains(&"xb"), "a conflicting edit was lost: {all:?}");
        assert!(!all.contains_key("시안/y.png"), "delete did not reach the late machine");
        assert_eq!(all.get("시안/z.png").map(String::as_str), Some("zb"), "edit must beat a concurrent delete");
        assert_eq!(all.values().filter(|s| *s == "xa" || *s == "xb").count(), 2, "conflict copy duplicated: {all:?}");

        // 다 맞춘 뒤에는 아무 일도 없어야 한다(되돌려 보내기 없음).
        let seqs: Vec<_> = m.iter().map(|x| x.index.clone()).collect();
        run(&mut m, &[(a, mm), (b, mm), (w, mm)]);
        assert_eq!(seqs, m.iter().map(|x| x.index.clone()).collect::<Vec<_>>());
    }
}
