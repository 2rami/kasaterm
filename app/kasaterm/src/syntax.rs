//! Tree-sitter syntax highlighting for the raw editor (E2). Replaces the
//! stateless per-line lexer for supported languages — multiline comments,
//! raw strings, and scope-accurate captures come from a real parse tree.
//! Unsupported / oversized buffers return None and the caller falls back
//! to `gpu::highlight_code_line`.

use std::cell::RefCell;
use std::collections::HashMap;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

/// Highlight class folded down to the existing six theme tokens + Base.
/// Spans cache the *kind*, never the color — `color()` resolves against the
/// live theme at draw time so a theme switch restyles cached spans instantly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SynKind {
    Base,
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
}

impl SynKind {
    pub(crate) fn color(self, base: [u8; 4]) -> [u8; 4] {
        use crate::theme;
        match self {
            SynKind::Base => base,
            SynKind::Keyword => theme::syn_keyword(),
            SynKind::String => theme::syn_string(),
            SynKind::Number => theme::syn_number(),
            SynKind::Comment => theme::syn_comment(),
            SynKind::Function => theme::syn_function(),
            SynKind::Type => theme::syn_type(),
        }
    }
}

/// Recognized capture names → kind. `configure()` assigns each query capture
/// (e.g. "function.method", "constant.builtin") the longest dot-prefix match
/// from this list, so first segments alone fold the whole taxonomy.
const CAPTURES: &[(&str, SynKind)] = &[
    ("attribute", SynKind::Type),
    ("boolean", SynKind::Number),
    ("comment", SynKind::Comment),
    ("constant", SynKind::Number),
    ("constructor", SynKind::Function),
    ("escape", SynKind::String),
    ("float", SynKind::Number),
    ("function", SynKind::Function),
    ("keyword", SynKind::Keyword),
    ("label", SynKind::Keyword),
    ("module", SynKind::Type),
    ("number", SynKind::Number),
    ("string", SynKind::String),
    ("tag", SynKind::Type),
    ("type", SynKind::Type),
];

/// Map a file extension or language name (call sites pass either) to the
/// canonical grammar key. None → unsupported, caller uses the line lexer.
pub(crate) fn canon_lang(lang: &str) -> Option<&'static str> {
    Some(match lang.to_ascii_lowercase().as_str() {
        "rust" | "rs" => "rust",
        "js" | "javascript" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "typescript" => "typescript",
        "tsx" => "tsx",
        "py" | "python" => "python",
        "json" | "jsonc" => "json",
        "sh" | "bash" | "zsh" | "shell" => "bash",
        "css" => "css",
        "html" | "htm" => "html",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        _ => return None,
    })
}

/// Full-reparse-per-edit gets expensive past this size; fall back to the
/// line lexer rather than risk keystroke latency on huge buffers.
const MAX_SOURCE_BYTES: usize = 1 << 20;

fn make_config(canon: &'static str) -> Option<HighlightConfiguration> {
    // TS/TSX highlight queries are written as a layer over the JS query
    // (upstream "inherits: ecma" convention), so concat with the TS side
    // first — its patterns must win on TS-specific syntax. The JSX query
    // only parses against grammars that have JSX nodes (javascript, tsx).
    let (language, highlights): (tree_sitter::Language, String) = match canon {
        "rust" => (tree_sitter_rust::LANGUAGE.into(), tree_sitter_rust::HIGHLIGHTS_QUERY.into()),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            format!(
                "{}\n{}",
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_javascript::HIGHLIGHT_QUERY
            ),
        ),
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!(
                "{}\n{}",
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                tree_sitter_javascript::HIGHLIGHT_QUERY
            ),
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            format!(
                "{}\n{}\n{}",
                tree_sitter_typescript::HIGHLIGHTS_QUERY,
                tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                tree_sitter_javascript::HIGHLIGHT_QUERY
            ),
        ),
        "python" => (tree_sitter_python::LANGUAGE.into(), tree_sitter_python::HIGHLIGHTS_QUERY.into()),
        "json" => (tree_sitter_json::LANGUAGE.into(), tree_sitter_json::HIGHLIGHTS_QUERY.into()),
        "bash" => (tree_sitter_bash::LANGUAGE.into(), tree_sitter_bash::HIGHLIGHT_QUERY.into()),
        "css" => (tree_sitter_css::LANGUAGE.into(), tree_sitter_css::HIGHLIGHTS_QUERY.into()),
        "html" => (tree_sitter_html::LANGUAGE.into(), tree_sitter_html::HIGHLIGHTS_QUERY.into()),
        "toml" => (tree_sitter_toml_ng::LANGUAGE.into(), tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into()),
        "yaml" => (tree_sitter_yaml::LANGUAGE.into(), tree_sitter_yaml::HIGHLIGHTS_QUERY.into()),
        _ => return None,
    };
    let mut cfg = HighlightConfiguration::new(language, canon, &highlights, "", "").ok()?;
    let names: Vec<&str> = CAPTURES.iter().map(|(n, _)| *n).collect();
    cfg.configure(&names);
    Some(cfg)
}

thread_local! {
    // Config compilation (query parse) is ~ms per language — do it once.
    // A failed build caches None so a broken grammar doesn't retry per frame.
    static CONFIGS: RefCell<HashMap<&'static str, Option<Box<HighlightConfiguration>>>> =
        RefCell::new(HashMap::new());
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

/// Highlight a whole buffer into per-line (token, kind) runs — the same shape
/// the raw-editor draw loop already consumes. Adjacent same-kind tokens are
/// merged to keep draw-call counts low.
pub(crate) fn highlight_lines(lang: &str, lines: &[String]) -> Option<Vec<Vec<(String, SynKind)>>> {
    let canon = canon_lang(lang)?;
    let source = lines.join("\n");
    if source.len() > MAX_SOURCE_BYTES {
        return None;
    }
    CONFIGS.with(|configs| {
        let mut configs = configs.borrow_mut();
        let cfg = configs
            .entry(canon)
            .or_insert_with(|| make_config(canon).map(Box::new))
            .as_deref()?;
        HIGHLIGHTER.with(|hl| {
            let mut hl = hl.borrow_mut();
            let events = hl.highlight(cfg, source.as_bytes(), None, |_| None).ok()?;
            let mut out: Vec<Vec<(String, SynKind)>> = vec![Vec::new(); lines.len()];
            let mut cur = 0usize;
            let mut stack: Vec<SynKind> = Vec::new();
            for ev in events {
                match ev.ok()? {
                    HighlightEvent::HighlightStart(h) => {
                        stack.push(CAPTURES.get(h.0).map_or(SynKind::Base, |(_, k)| *k));
                    }
                    HighlightEvent::HighlightEnd => {
                        stack.pop();
                    }
                    HighlightEvent::Source { start, end } => {
                        let kind = stack.last().copied().unwrap_or(SynKind::Base);
                        let text = source.get(start..end)?;
                        for (i, part) in text.split('\n').enumerate() {
                            if i > 0 {
                                cur += 1;
                            }
                            if part.is_empty() {
                                continue;
                            }
                            let Some(row) = out.get_mut(cur) else { continue };
                            match row.last_mut() {
                                Some(last) if last.1 == kind => last.0.push_str(part),
                                _ => row.push((part.to_string(), kind)),
                            }
                        }
                    }
                }
            }
            Some(out)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(src: &str) -> Vec<String> {
        src.lines().map(str::to_string).collect()
    }

    fn kinds_for(row: &[(String, SynKind)]) -> Vec<(&str, SynKind)> {
        row.iter().map(|(t, k)| (t.as_str(), *k)).collect()
    }

    #[test]
    fn rust_keywords_strings_comments() {
        let src = lines("fn main() {\n    let s = \"hi\"; // note\n}");
        let hl = highlight_lines("rs", &src).expect("rust grammar");
        assert_eq!(hl.len(), 3);
        // Line 0: `fn` keyword, `main` function.
        assert!(hl[0].iter().any(|(t, k)| t == "fn" && *k == SynKind::Keyword), "{:?}", kinds_for(&hl[0]));
        assert!(hl[0].iter().any(|(t, k)| t == "main" && *k == SynKind::Function), "{:?}", kinds_for(&hl[0]));
        // Line 1: string literal and trailing comment.
        assert!(hl[1].iter().any(|(t, k)| t.contains("hi") && *k == SynKind::String), "{:?}", kinds_for(&hl[1]));
        assert!(hl[1].iter().any(|(t, k)| t.contains("note") && *k == SynKind::Comment), "{:?}", kinds_for(&hl[1]));
    }

    #[test]
    fn rust_multiline_comment_spans_lines() {
        // The stateless line lexer could never mark line 2 of a block comment —
        // this is the core upgrade tree-sitter buys us.
        let src = lines("/* first\nsecond */\nfn x() {}");
        let hl = highlight_lines("rust", &src).expect("rust grammar");
        assert!(hl[0].iter().all(|(_, k)| *k == SynKind::Comment), "{:?}", kinds_for(&hl[0]));
        assert!(hl[1].iter().all(|(_, k)| *k == SynKind::Comment), "{:?}", kinds_for(&hl[1]));
        assert!(hl[2].iter().any(|(t, k)| t == "fn" && *k == SynKind::Keyword), "{:?}", kinds_for(&hl[2]));
    }

    #[test]
    fn line_reassembly_preserves_text() {
        // Concatenating each row's tokens must reproduce the source line
        // exactly — draw advances pen-x per token, any drift garbles layout.
        let src = lines("use std::collections::HashMap;\n\nconst N: usize = 42; // answer");
        let hl = highlight_lines("rust", &src).expect("rust grammar");
        for (li, line) in src.iter().enumerate() {
            let joined: String = hl[li].iter().map(|(t, _)| t.as_str()).collect();
            assert_eq!(&joined, line, "line {li}");
        }
    }

    #[test]
    fn unsupported_and_oversized_fall_back() {
        assert!(highlight_lines("brainfuck", &lines("+++")).is_none());
        let huge = vec!["x".repeat(MAX_SOURCE_BYTES + 1)];
        assert!(highlight_lines("rust", &huge).is_none());
    }

    #[test]
    fn ts_and_tsx_configs_build() {
        // The concat'd inheritance queries must survive query compilation
        // against their grammars — a QueryError here would silently fall
        // back to the line lexer for every .ts/.tsx file.
        let src = lines("const n: number = 1;");
        assert!(highlight_lines("ts", &src).is_some());
        assert!(highlight_lines("tsx", &src).is_some());
        for l in ["js", "py", "json", "sh", "css", "html", "toml", "yaml"] {
            assert!(highlight_lines(l, &lines("x")).is_some(), "{l} grammar failed to build");
        }
    }
}
