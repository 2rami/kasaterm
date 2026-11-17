//! 기록에서 화면에 놓을 트리를 계산한다(기록은 고쳐 쓰지 않는다). 모든 기기가 같은 기록에서 같은 트리를 얻으므로
//! 고아·고리를 저마다 고쳐 쓰며 다투지 않는다.

use std::collections::{BTreeMap, BTreeSet};

use super::record::MarkRecord;

/// 부모 동기화 id(빈 값은 맨 위)마다 자식 동기화 id 를 자리 순서로.
pub type Layout = BTreeMap<String, Vec<String>>;

/// 살아 있는 기록들의 트리. 부모가 없거나 지워졌거나 폴더가 아니면 맨 위에 둔다. 고리는 그 안에서 자리를 가장 늦게 바꾼
/// 항목을 맨 위로 빼서 끊는다. 형제는 `(pos, 동기화 id)` 순서.
pub fn layout(records: &BTreeMap<String, MarkRecord>) -> Layout {
    // 내용을 모르는 살아 있는 북마크(주소 없는 것)는 놓을 수 없어 뺀다.
    let alive: BTreeMap<&str, &MarkRecord> =
        records.values().filter(|r| r.alive() && (r.folder || !r.url.is_empty())).map(|r| (r.sync_id.as_str(), r)).collect();
    let mut parent: BTreeMap<&str, &str> = alive
        .iter()
        .map(|(id, r)| {
            let p = r.parent.as_str();
            let ok = !p.is_empty() && p != *id && alive.get(p).is_some_and(|f| f.folder);
            (*id, if ok { p } else { "" })
        })
        .collect();
    loop {
        let Some(cycle) = find_cycle(&parent) else { break };
        let breaker = cycle.iter().max_by(|a, b| (&alive[*a].place, *a).cmp(&(&alive[*b].place, *b))).copied().unwrap_or("");
        parent.insert(breaker, "");
    }
    let mut out: Layout = BTreeMap::new();
    for (id, p) in &parent {
        out.entry((*p).to_string()).or_default().push((*id).to_string());
    }
    for children in out.values_mut() {
        children.sort_by(|a, b| (&alive[a.as_str()].pos, a).cmp(&(&alive[b.as_str()].pos, b)));
    }
    out
}

fn find_cycle<'a>(parent: &BTreeMap<&'a str, &'a str>) -> Option<Vec<&'a str>> {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for start in parent.keys() {
        let mut path: Vec<&str> = Vec::new();
        let mut at = *start;
        while !at.is_empty() && !done.contains(at) {
            if let Some(i) = path.iter().position(|p| *p == at) {
                return Some(path[i..].to_vec());
            }
            path.push(at);
            at = parent.get(at).copied().unwrap_or("");
        }
        done.extend(path);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_sync::record::Stamp;

    fn rec(id: &str, parent: &str, pos: &str, place_at: u64, folder: bool) -> (String, MarkRecord) {
        (id.to_string(), MarkRecord {
            collection: "c".into(),
            sync_id: id.into(),
            folder,
            url: if folder { String::new() } else { format!("https://{id}.kasa-test.example/") },
            content: Stamp::new(1, "a"),
            parent: parent.into(),
            pos: pos.into(),
            place: Stamp::new(place_at, "a"),
            ..Default::default()
        })
    }

    #[test]
    fn orphans_go_to_the_top_and_order_is_by_place() {
        let records: BTreeMap<_, _> = [
            rec("f", "", "M", 1, true),
            rec("b", "f", "V", 1, false),
            rec("a", "f", "F", 1, false),
            rec("lost", "gone", "A", 1, false),
            rec("in-mark", "a", "A", 1, false),
        ]
        .into_iter()
        .collect();
        let tree = layout(&records);
        assert_eq!(tree["f"], vec!["a", "b"]);
        assert_eq!(tree[""], vec!["in-mark", "lost", "f"], "없는 부모·폴더 아닌 부모면 맨 위, 같은 자리는 id 순");
    }

    #[test]
    fn a_cycle_breaks_at_its_latest_move() {
        // 두 기기가 x 를 y 안으로, y 를 x 안으로 옮겼다 — 나중에 옮긴 y 가 맨 위로.
        let records: BTreeMap<_, _> = [rec("x", "y", "M", 5, true), rec("y", "x", "M", 9, true)].into_iter().collect();
        let tree = layout(&records);
        assert_eq!(tree[""], vec!["y"]);
        assert_eq!(tree["y"], vec!["x"]);
    }

    #[test]
    fn deleted_marks_are_left_out() {
        let mut records: BTreeMap<_, _> = [rec("f", "", "M", 1, true), rec("a", "f", "F", 1, false)].into_iter().collect();
        records.get_mut("f").unwrap().deleted = Some(Stamp::new(9, "b"));
        let tree = layout(&records);
        assert_eq!(tree[""], vec!["a"], "지운 폴더의 살아 있는 자식은 맨 위로");
        assert!(!tree.contains_key("f"));
    }
}
