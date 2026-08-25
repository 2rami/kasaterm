//! 편집기 거터의 **HEAD 대비 실시간 diff**.
//!
//! git 컬럼의 인라인 diff(`kasa_mcp::git::git_file_diff`)를 그대로 못 쓴다 — 그건
//! `git diff` 라 **디스크**를 보고, 저장 전 버퍼를 모른다. 「타이핑하는 동안 바가
//! 따라 움직인다」가 이 기능의 전부라서, 원본만 git 에서 받아 오고(`git_head_text`)
//! 차이는 여기서 메모리로 낸다.
//!
//! 결과가 없는 상태(레포 밖·미추적·변경 없음)를 `None` 으로 두는 규율은 접힘
//! (`folds`)·보조커서(`extra`)와 같다 — 그때 거터 루프 비용이 정확히 0 이라야
//! 이 기능이 없던 때와 편집 감각이 같다.

use std::ops::Range;

/// 버퍼 한 줄이 HEAD 대비 어떤 상태인지. 거터의 색 바 하나가 이 한 칸이다.
///
/// 삭제는 여기 없다 — 지워진 줄은 지금 버퍼에 **자리가 없어서** 줄이 아니라
/// 줄과 줄 *사이*에 표시된다(`BufferDiff::dels`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LineMark {
    Add,
    Mod,
}

/// 변경 덩어리 하나. 펼쳐보기와 되돌리기가 이 단위로 움직인다.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Hunk {
    /// 지금 버퍼에서 이 헝크가 덮는 줄 범위. **빈 범위 = 순수 삭제**(버퍼에 흔적이
    /// 없고 `dels` 에만 자리가 남는다).
    pub(crate) new: Range<usize>,
    /// HEAD 쪽 원본 줄들. **비어 있으면 순수 추가**(되돌리면 그냥 지워진다).
    pub(crate) old: Vec<String>,
}

/// 버퍼 하나의 diff 결과.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct BufferDiff {
    /// 줄 인덱스 → 마커. 헝크를 프레임마다 이진탐색하지 않으려고 한 번 펴 둔다 —
    /// 그리기 루프는 화면에 보이는 줄마다 이걸 O(1) 로 조회만 한다.
    pub(crate) marks: Vec<Option<LineMark>>,
    /// 지워진 자리 — 값은 「이 버퍼 줄 **바로 앞**에서 줄이 사라졌다」. 파일 끝에서
    /// 지워졌으면 `lines.len()` 과 같아서, 그릴 때는 마지막 줄의 아래 변이 된다.
    pub(crate) dels: Vec<usize>,
    /// 덮는 버퍼 줄 순으로 정렬. 클릭 → 헝크 찾기가 이 순서를 전제한다.
    pub(crate) hunks: Vec<Hunk>,
    /// 이 결과를 만든 버퍼 세대(`MarkdownPane::edit_gen`). 지금 세대와 같으면
    /// 다시 뜨지 않는다.
    pub(crate) gen: u64,
}

impl BufferDiff {
    pub(crate) fn is_empty(&self) -> bool {
        self.hunks.is_empty() && self.dels.is_empty()
    }

    /// 이 버퍼 줄을 덮는 헝크. 순수 삭제 헝크(빈 범위)는 그 자리의 줄이 자기
    /// 것이 아니므로 `contains` 로 안 잡히고, 그래서 따로 본다.
    pub(crate) fn hunk_at(&self, line: usize) -> Option<&Hunk> {
        self.hunks
            .iter()
            .find(|h| h.new.contains(&line) || (h.new.is_empty() && h.new.start == line))
    }
}

/// 원본과 버퍼의 줄 차이. `gen` 은 이 버퍼의 세대 — 결과에 그대로 실어 둔다.
///
/// 시간 상한을 거는 이유: 이 함수는 GUI 스레드의 틱에서 돈다. 보통 파일은 몇
/// ms 면 끝나지만 병적인 입력(수만 줄이 통째로 뒤섞인 경우)에서 Myers 는 오래
/// 걸릴 수 있고, 그러면 그 프레임이 통째로 멎는다. 상한에 걸리면 `similar` 는
/// 실패가 아니라 **더 거친 답**을 내므로 화면은 계속 성립한다.
pub(crate) fn diff_lines(old: &[String], new: &[String], gen: u64) -> BufferDiff {
    use similar::{Algorithm, DiffOp};

    let mut out = BufferDiff { marks: vec![None; new.len()], gen, ..Default::default() };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(50);
    let ops = similar::capture_diff_slices_deadline(Algorithm::Myers, old, new, Some(deadline));

    for op in &ops {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => {
                let range = new_index..new_index + new_len;
                for m in &mut out.marks[range.clone()] {
                    *m = Some(LineMark::Add);
                }
                out.hunks.push(Hunk { new: range, old: Vec::new() });
            }
            DiffOp::Delete { old_index, old_len, new_index } => {
                out.dels.push(new_index);
                out.hunks.push(Hunk {
                    new: new_index..new_index,
                    old: old[old_index..old_index + old_len].to_vec(),
                });
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                let range = new_index..new_index + new_len;
                for m in &mut out.marks[range.clone()] {
                    *m = Some(LineMark::Mod);
                }
                out.hunks.push(Hunk {
                    new: range,
                    old: old[old_index..old_index + old_len].to_vec(),
                });
            }
        }
    }
    out
}

/// `git show` 가 준 본문을 줄 배열로.
///
/// ⚠️ **편집기 버퍼와 글자 그대로 같은 방식이라야 한다** — `markdown.rs` 가
/// `doc.raw.split('\n')` 로 만든다. 여기서만 「끝 개행은 빈 줄로 안 친다」거나
/// 「`\r` 을 떼자」처럼 영리하게 굴면, 두 배열의 마지막이 어긋나 **고치지도 않은
/// 파일 끝에 늘 가짜 헝크 하나**가 붙는다(실측: 마커 3 이 나와야 할 파일에서
/// 4 가 나왔다). 다듬을 데가 있다면 양쪽을 같이 옮겨야 한다.
pub(crate) fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(String::from).collect()
}

/// HEAD 원본을 읽어 본 **결과**. 「아직 안 읽음」(`Option::None`)과 「읽었는데
/// 없음」(`Absent`)을 반드시 갈라야 한다 — 하나로 뭉치면 레포 밖 파일이나 새
/// 파일에서 **틱마다 `git show` 프로세스가 새로 뜬다.**
#[derive(Clone)]
pub(crate) enum HeadText {
    /// HEAD 에 이 경로가 없다(미추적·새 파일·레포 밖). 다시 묻지 않는다.
    Absent,
    /// 원본 줄 배열. `Arc` 인 이유는 `edit_lines` 와 같다 — 갱신 판정 때마다
    /// 락 안에서 한 번씩 떠 가는데, 그때 파일 전체를 복사할 이유가 없다.
    Lines(std::sync::Arc<Vec<String>>),
}

/// 이 절대경로의 HEAD 원본을 읽는다.
///
/// 레포 루트는 경로를 거슬러 `.git` 을 찾아 정한다 — `lsp_attach` 가 `Cargo.toml`
/// 로 프로젝트 루트를 잡는 것과 같은 손이다. git 컬럼의 `cwd` 를 빌리지 않는
/// 이유는 그게 **활성 pane 을 따라다니는** 값이라, 편집기가 다른 레포의 파일을
/// 열고 있으면 엉뚱한 레포에 대고 묻게 되기 때문이다.
///
/// ⚠️ `.git` 은 워크트리·서브모듈에서 디렉터리가 아니라 **파일**이다. `is_dir`
/// 로 보면 그런 체크아웃에서 표시가 통째로 안 뜬다.
pub(crate) fn read_head_text(path: &str) -> HeadText {
    let p = std::path::Path::new(path);
    let Some(root) = p.ancestors().skip(1).find(|d| d.join(".git").exists()) else {
        return HeadText::Absent;
    };
    let Ok(rel) = p.strip_prefix(root) else { return HeadText::Absent };
    // git 은 경로 구분자로 `/` 만 받는다 — 윈도우의 `\` 를 그대로 넘기면
    // 「그런 경로 없음」이 되어 표시가 조용히 사라진다.
    let rel = rel.components().filter_map(|c| c.as_os_str().to_str()).collect::<Vec<_>>().join("/");
    match kasa_mcp::git::git_head_text(root, &rel) {
        Some(text) => HeadText::Lines(std::sync::Arc::new(split_lines(&text))),
        None => HeadText::Absent,
    }
}

/// 그리기 쪽에 넘기는 얇은 뷰. `draw_raw_editor` 는 이미 인자가 스무 개라
/// 마커·삭제자리·펼침을 낱개로 더하지 않는다.
pub(crate) struct DiffView<'a> {
    pub(crate) marks: &'a [Option<LineMark>],
    pub(crate) dels: &'a [usize],
    /// 펼친 헝크 — `(붙일 버퍼 줄, 지워진 원본 줄들)`. 패널은 그 줄 **아래**에 뜬다.
    pub(crate) peek: Option<(usize, &'a [String])>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn 추가만_초록_마커와_헝크() {
        let d = diff_lines(&v(&["a", "b"]), &v(&["a", "x", "y", "b"]), 7);
        assert_eq!(d.marks, vec![None, Some(LineMark::Add), Some(LineMark::Add), None]);
        assert!(d.dels.is_empty());
        assert_eq!(d.hunks, vec![Hunk { new: 1..3, old: Vec::new() }]);
        assert_eq!(d.gen, 7);
    }

    #[test]
    fn 삭제는_줄이_아니라_줄사이에_남는다() {
        let d = diff_lines(&v(&["a", "b", "c"]), &v(&["a", "c"]), 0);
        // 버퍼에 자리가 없으므로 어느 줄도 마커를 안 받는다.
        assert_eq!(d.marks, vec![None, None]);
        // "b" 가 지워진 자리는 지금 버퍼의 1번 줄("c") 바로 앞.
        assert_eq!(d.dels, vec![1]);
        assert_eq!(d.hunks, vec![Hunk { new: 1..1, old: v(&["b"]) }]);
    }

    #[test]
    fn 파일끝_삭제는_마지막줄_뒤를_가리킨다() {
        let d = diff_lines(&v(&["a", "b"]), &v(&["a"]), 0);
        // len() 과 같은 값 = 마지막 줄의 아래 변에 그린다.
        assert_eq!(d.dels, vec![1]);
        assert_eq!(d.hunks[0].new, 1..1);
    }

    #[test]
    fn 수정은_파랑_마커에_원본을_들고_있다() {
        let d = diff_lines(&v(&["a", "b", "c"]), &v(&["a", "B", "c"]), 0);
        assert_eq!(d.marks, vec![None, Some(LineMark::Mod), None]);
        assert!(d.dels.is_empty(), "수정은 삭제 쐐기를 만들지 않는다");
        assert_eq!(d.hunks, vec![Hunk { new: 1..2, old: v(&["b"]) }]);
    }

    #[test]
    fn 변경이_없으면_빈_결과() {
        let d = diff_lines(&v(&["a", "b"]), &v(&["a", "b"]), 0);
        assert!(d.is_empty());
        assert_eq!(d.marks, vec![None, None]);
    }

    #[test]
    fn 빈_파일에_처음_쓴_경우() {
        let d = diff_lines(&[], &v(&["a"]), 0);
        assert_eq!(d.marks, vec![Some(LineMark::Add)]);
        assert_eq!(d.hunks, vec![Hunk { new: 0..1, old: Vec::new() }]);
    }

    #[test]
    fn 통째로_지운_경우_삭제자리는_맨앞() {
        let d = diff_lines(&v(&["a", "b"]), &[], 0);
        assert!(d.marks.is_empty());
        assert_eq!(d.dels, vec![0]);
    }

    #[test]
    fn hunk_at_은_순수삭제도_집는다() {
        let d = diff_lines(&v(&["a", "b", "c"]), &v(&["a", "c"]), 0);
        // 1번 줄("c")을 누르면 그 앞에서 지워진 헝크가 잡혀야 한다.
        assert_eq!(d.hunk_at(1).map(|h| h.old.clone()), Some(v(&["b"])));
        assert!(d.hunk_at(0).is_none());
    }

    #[test]
    fn split_lines_는_편집기_버퍼와_같은_모양이다() {
        // 기준은 `markdown.rs` 의 `doc.raw.split('\n')` — 그것과 한 글자도
        // 다르면 파일 끝에 가짜 헝크가 선다.
        let same = |t: &str| assert_eq!(split_lines(t), t.split('\n').map(String::from).collect::<Vec<_>>());
        for t in ["a\nb\n", "a\nb", "", "\n", "a\r\nb\r\n"] {
            same(t);
        }
        // 끝 개행이 빈 줄 하나를 만든다는 사실 자체를 못 박아 둔다.
        assert_eq!(split_lines("a\nb\n"), v(&["a", "b", ""]));
    }
}
