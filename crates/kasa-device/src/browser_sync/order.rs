//! 형제 사이 순서. 자리(`pos`)는 언제나 두 값 사이에 새 값을 끼울 수 있는 글자열이라, 하나를 옮겨도 형제 전체를
//! 다시 쓰지 않는다. 로컬에서 순서가 바뀌면 아는 자리 순서의 최장 증가 부분열 밖에 있는 항목만 새 자리를 받는다.

const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE: usize = DIGITS.len();

fn digit(c: u8) -> usize {
    DIGITS.iter().position(|d| *d == c).unwrap_or(0)
}

/// `a` 와 `b` 사이의 자리. `b` 가 비면 끝없이 뒤, `a` 가 비면 끝없이 앞이다. 만든 자리는 `0` 으로 끝나지 않아
/// 그 앞에도 언제나 끼울 수 있다. `a < b` 가 아니면 `a` 뒤의 자리를 준다.
pub fn between(a: &str, b: &str) -> String {
    let a: Vec<usize> = a.bytes().map(digit).collect();
    let b: Vec<usize> = b.bytes().map(digit).collect();
    let upper = if b.is_empty() || b <= a { None } else { Some(b.as_slice()) };
    midpoint(&a, upper).into_iter().map(|d| DIGITS[d] as char).collect()
}

fn midpoint(a: &[usize], b: Option<&[usize]>) -> Vec<usize> {
    // 남이 보낸 자리는 `F` 와 `F0` 처럼 사이가 없을 수 있다 — 그때는 뒤에 둔다.
    let b = b.filter(|b| !b.is_empty());
    if let Some(b) = b {
        let mut n = 0;
        while n < b.len() && a.get(n).copied().unwrap_or(0) == b[n] {
            n += 1;
        }
        if n > 0 {
            let mut out = b[..n].to_vec();
            out.extend(midpoint(a.get(n..).unwrap_or(&[]), Some(&b[n..])));
            return out;
        }
    }
    let low = a.first().copied().unwrap_or(0);
    let high = b.map_or(BASE, |b| b[0]);
    // 끝에 붙이거나 맨 앞에 끼울 때는 반으로 가르지 않고 한 칸씩 간다 — 반씩 가르면 몇 번 만에 끝 글자에 닿아
    // 붙일 때마다 자리가 길어진다.
    if b.is_none() && !a.is_empty() && low + 1 < BASE {
        return vec![low + 1];
    }
    if a.is_empty() && b.is_some() && high > 1 {
        return vec![high - 1];
    }
    if high - low > 1 {
        return vec![(low + high) / 2];
    }
    if let Some(b) = b.filter(|b| b.len() > 1) {
        return vec![b[0]];
    }
    let mut out = vec![low];
    out.extend(midpoint(a.get(1..).unwrap_or(&[]), None));
    out
}

/// 형제 하나. `pos` 는 아는 자리(그 자리 그대로 둘 수 있을 때만), 없으면 새 자리가 필요하다.
pub struct Sibling<'a> {
    pub id: &'a str,
    pub pos: Option<&'a str>,
}

/// 로컬 순서대로 놓인 형제에게 자리를 준다. 아는 자리의 최장 증가 부분열은 그대로 두고, 나머지만 이웃 사이 새 자리를 받는다.
/// 돌려주는 것은 형제마다 `(자리, 새로 받았나)`.
pub fn place(siblings: &[Sibling]) -> Vec<(String, bool)> {
    let keep = longest_increasing(siblings);
    let mut out: Vec<(String, bool)> = Vec::with_capacity(siblings.len());
    let mut previous = String::new();
    let mut i = 0;
    while i < siblings.len() {
        if keep[i] {
            let pos = siblings[i].pos.unwrap_or_default().to_string();
            previous = pos.clone();
            out.push((pos, false));
            i += 1;
            continue;
        }
        let end = (i..siblings.len()).find(|j| keep[*j]).unwrap_or(siblings.len());
        let next = siblings.get(end).and_then(|s| s.pos).unwrap_or("");
        for pos in spread(&previous, next, end - i) {
            out.push((pos, true));
        }
        previous = out.last().map(|p| p.0.clone()).unwrap_or_default();
        i = end;
    }
    out
}

/// `a` 와 `b` 사이에 `n` 개의 자리를 순서대로. 위가 막혀 있으면 가운데부터 반씩 갈라 자리가 `log n` 만큼만 길어진다.
fn spread(a: &str, b: &str, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    if b.is_empty() {
        let mut out = Vec::with_capacity(n);
        let mut previous = a.to_string();
        for _ in 0..n {
            previous = between(&previous, "");
            out.push(previous.clone());
        }
        return out;
    }
    let middle = between(a, b);
    let left = (n - 1) / 2;
    let mut out = spread(a, &middle, left);
    out.push(middle.clone());
    out.extend(spread(&middle, b, n - 1 - left));
    out
}

/// 아는 자리가 엄격히 커지는 가장 긴 부분열에 드는가. 같은 자리 둘은 함께 남지 못해 하나가 새 자리를 받는다.
fn longest_increasing(siblings: &[Sibling]) -> Vec<bool> {
    let mut tails: Vec<usize> = Vec::new();
    let mut back: Vec<Option<usize>> = vec![None; siblings.len()];
    for (i, s) in siblings.iter().enumerate() {
        let Some(pos) = s.pos else { continue };
        let at = tails.partition_point(|t| siblings[*t].pos.unwrap_or_default() < pos);
        back[i] = at.checked_sub(1).map(|p| tails[p]);
        if at == tails.len() {
            tails.push(i);
        } else {
            tails[at] = i;
        }
    }
    let mut keep = vec![false; siblings.len()];
    let mut at = tails.last().copied();
    while let Some(i) = at {
        keep[i] = true;
        at = back[i];
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_always_fits_between() {
        let mut seed = 7u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            seed >> 33
        };
        let mut keys = vec![between("", "")];
        for _ in 0..2000 {
            let i = (next() as usize) % (keys.len() + 1);
            let a = if i == 0 { String::new() } else { keys[i - 1].clone() };
            let b = keys.get(i).cloned().unwrap_or_default();
            let c = between(&a, &b);
            assert!(c.as_str() > a.as_str() && (b.is_empty() || c.as_str() < b.as_str()), "{a:?} < {c:?} < {b:?}");
            assert!(!c.ends_with('0'));
            keys.insert(i, c);
        }
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert!(keys.iter().all(|k| k.len() < 40), "자리가 끝없이 길어지지 않는다");
        assert!(between("b", "a").as_str() > "b", "거꾸로 주면 a 뒤");
    }

    #[test]
    fn places_stay_short_when_marks_pile_up_at_one_end_or_in_one_gap() {
        let mut last = String::new();
        let mut first = String::new();
        for _ in 0..1000 {
            let next = between(&last, "");
            assert!(next > last);
            last = next;
            let before = between("", &first);
            assert!(first.is_empty() || before < first);
            first = before;
        }
        assert!(last.len() <= 40 && first.len() <= 40, "끝에 1000번 붙여도 짧다: {} {}", last.len(), first.len());
        let gap = spread("F", "G", 1000);
        assert!(gap.windows(2).all(|w| w[0] < w[1]) && gap[0].as_str() > "F" && gap[999].as_str() < "G");
        assert!(gap.iter().all(|k| k.len() <= 8), "한 틈에 1000개도 반씩 갈라 짧다");
    }

    #[test]
    fn only_the_moved_ones_get_a_new_place() {
        let s = |id, pos| Sibling { id, pos: Some(pos) };
        // c 를 맨 앞으로 옮겼다: a b 는 그대로, c 만 새 자리.
        let placed = place(&[s("c", "V"), s("a", "F"), s("b", "M")]);
        assert_eq!(placed.iter().map(|p| p.1).collect::<Vec<_>>(), vec![true, false, false]);
        assert!(placed[0].0.as_str() < "F");
        // 새 항목 둘이 a 와 b 사이에.
        let placed = place(&[s("a", "F"), Sibling { id: "n1", pos: None }, Sibling { id: "n2", pos: None }, s("b", "M")]);
        assert!(placed[1].0.as_str() > "F" && placed[1].0 < placed[2].0 && placed[2].0.as_str() < "M");
        assert!(!placed[0].1 && placed[1].1 && placed[2].1 && !placed[3].1);
        // 같은 자리 둘은 하나만 남는다.
        let placed = place(&[s("a", "F"), s("b", "F")]);
        assert!(placed[0].0 < placed[1].0);
    }
}
