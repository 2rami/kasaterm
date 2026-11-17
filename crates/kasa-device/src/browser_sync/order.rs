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
    for (i, s) in siblings.iter().enumerate() {
        if keep[i] {
            let pos = s.pos.unwrap_or_default().to_string();
            previous = pos.clone();
            out.push((pos, false));
            continue;
        }
        let next = (i + 1..siblings.len()).find(|j| keep[*j]).and_then(|j| siblings[j].pos).unwrap_or("");
        let pos = between(&previous, next);
        previous = pos.clone();
        out.push((pos, true));
    }
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
