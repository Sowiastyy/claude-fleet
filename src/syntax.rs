//! Colouring source code, well enough to read it: keywords, strings,
//! comments, numbers, and names that look like types or calls.
//!
//! This is a tokenizer per line, not a parser. The only state carried from
//! one line to the next is whether a block comment is still open, which is
//! what makes a half-screen of commented-out code read as a comment.

use std::path::Path;

pub struct Lang {
    pub name: &'static str,
    pub line_comment: Option<&'static str>,
    pub block_comment: Option<(&'static str, &'static str)>,
    pub quotes: &'static [char],
    pub keywords: &'static [&'static str],
    /// Keywords match whatever the case, as in SQL.
    pub fold_case: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tok {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
    /// A capitalised name: a type, a constructor, a constant.
    Type,
    /// A name followed by `(`.
    Call,
    /// A Markdown heading, or similar structure worth standing out.
    Heading,
}

const RUST: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while",
];
const JS: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "of",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];
const PY: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "self", "try",
    "while", "with", "yield",
];
const GO: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "false",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "nil",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "true",
    "type",
    "var",
];
const C: &[&str] = &[
    "auto",
    "bool",
    "break",
    "case",
    "char",
    "class",
    "const",
    "constexpr",
    "continue",
    "default",
    "define",
    "delete",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "false",
    "float",
    "for",
    "goto",
    "if",
    "include",
    "inline",
    "int",
    "long",
    "namespace",
    "new",
    "nullptr",
    "NULL",
    "override",
    "private",
    "protected",
    "public",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "template",
    "this",
    "true",
    "typedef",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];
const JAVA: &[&str] = &[
    "abstract",
    "async",
    "await",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "false",
    "final",
    "finally",
    "float",
    "for",
    "fun",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "int",
    "interface",
    "internal",
    "is",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "sealed",
    "short",
    "static",
    "string",
    "super",
    "switch",
    "this",
    "throw",
    "throws",
    "true",
    "try",
    "using",
    "val",
    "var",
    "void",
    "volatile",
    "when",
    "while",
];
const SH: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in",
    "local", "return", "then", "until", "while",
];
const PS: &[&str] = &[
    "begin", "break", "catch", "continue", "do", "else", "elseif", "end", "exit", "filter",
    "finally", "for", "foreach", "function", "if", "in", "param", "process", "return", "switch",
    "throw", "try", "until", "while",
];
const SQL: &[&str] = &[
    "add", "all", "alter", "and", "as", "asc", "begin", "between", "by", "case", "commit",
    "create", "delete", "desc", "distinct", "drop", "else", "end", "exists", "from", "group",
    "having", "in", "index", "inner", "insert", "into", "is", "join", "key", "left", "like",
    "limit", "not", "null", "on", "or", "order", "outer", "primary", "right", "select", "set",
    "table", "then", "union", "update", "values", "when", "where", "with",
];
const LITERALS: &[&str] = &["true", "false", "null"];

macro_rules! lang {
    ($name:expr, $line:expr, $block:expr, $quotes:expr, $kw:expr) => {
        Lang {
            name: $name,
            line_comment: $line,
            block_comment: $block,
            quotes: $quotes,
            keywords: $kw,
            fold_case: false,
        }
    };
}

const SLASH_BLOCK: Option<(&str, &str)> = Some(("/*", "*/"));

static PLAIN: Lang = lang!("Text", None, None, &[], &[]);
static L_RUST: Lang = lang!("Rust", Some("//"), SLASH_BLOCK, &['"', '\''], RUST);
static L_JS: Lang = lang!("JavaScript", Some("//"), SLASH_BLOCK, &['"', '\'', '`'], JS);
static L_TS: Lang = lang!("TypeScript", Some("//"), SLASH_BLOCK, &['"', '\'', '`'], JS);
static L_PY: Lang = lang!("Python", Some("#"), None, &['"', '\''], PY);
static L_GO: Lang = lang!("Go", Some("//"), SLASH_BLOCK, &['"', '\'', '`'], GO);
static L_C: Lang = lang!("C/C++", Some("//"), SLASH_BLOCK, &['"', '\''], C);
static L_JAVA: Lang = lang!(
    "Java/C#/Kotlin",
    Some("//"),
    SLASH_BLOCK,
    &['"', '\''],
    JAVA
);
static L_SH: Lang = lang!("Shell", Some("#"), None, &['"', '\''], SH);
static L_PS: Lang = lang!(
    "PowerShell",
    Some("#"),
    Some(("<#", "#>")),
    &['"', '\''],
    PS
);
static L_BAT: Lang = lang!("Batch", Some("REM "), None, &['"'], &[]);
static L_TOML: Lang = lang!("TOML", Some("#"), None, &['"', '\''], LITERALS);
static L_YAML: Lang = lang!("YAML", Some("#"), None, &['"', '\''], LITERALS);
static L_JSON: Lang = lang!("JSON", None, None, &['"'], LITERALS);
static L_CSS: Lang = lang!("CSS", None, SLASH_BLOCK, &['"', '\''], &[]);
static L_HTML: Lang = lang!("HTML", None, Some(("<!--", "-->")), &['"', '\''], &[]);
static L_MD: Lang = lang!("Markdown", None, Some(("<!--", "-->")), &['`'], &[]);
static L_SQL: Lang = Lang {
    fold_case: true,
    ..lang!("SQL", Some("--"), SLASH_BLOCK, &['\'', '"'], SQL)
};

/// The language a file is in, by its extension.
pub fn detect(path: &Path) -> &'static Lang {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "rs" => &L_RUST,
        "js" | "mjs" | "cjs" | "jsx" => &L_JS,
        "ts" | "tsx" | "mts" | "cts" => &L_TS,
        "py" | "pyw" | "pyi" => &L_PY,
        "go" => &L_GO,
        "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => &L_C,
        "java" | "cs" | "kt" | "kts" | "scala" | "swift" | "dart" => &L_JAVA,
        "sh" | "bash" | "zsh" => &L_SH,
        "ps1" | "psm1" | "psd1" => &L_PS,
        "bat" | "cmd" => &L_BAT,
        "toml" | "ini" | "cfg" | "conf" => &L_TOML,
        "yml" | "yaml" => &L_YAML,
        "json" | "jsonc" | "json5" => &L_JSON,
        "css" | "scss" | "less" => &L_CSS,
        "html" | "htm" | "xml" | "svg" | "vue" | "svelte" => &L_HTML,
        "md" | "markdown" => &L_MD,
        "sql" => &L_SQL,
        _ if name == "dockerfile" || name == "makefile" || name.starts_with(".env") => &L_SH,
        _ if name == ".gitignore" || name == ".gitattributes" => &L_SH,
        _ => &PLAIN,
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn starts_with_at(chars: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, p)| chars.get(i + k) == Some(&p))
}

/// A token per character of `line`, and whether a block comment is still
/// open at its end. `in_block` is the same, for the line before.
pub fn highlight(line: &str, lang: &Lang, mut in_block: bool) -> (Vec<Tok>, bool) {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out = vec![Tok::Plain; n];

    if lang.name == "Markdown" && !in_block {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with("```") {
            out.fill(Tok::Heading);
            return (out, false);
        }
    }

    let mut i = 0;
    while i < n {
        if in_block {
            let (_, close) = lang.block_comment.expect("only set with a block comment");
            let start = i;
            while i < n && !starts_with_at(&chars, i, close) {
                i += 1;
            }
            if i < n {
                i += close.chars().count();
                in_block = false;
            }
            out[start..i.min(n)].fill(Tok::Comment);
            continue;
        }
        let c = chars[i];
        if let Some(lc) = lang.line_comment {
            let hit = if lc == "REM " {
                // Batch comments only start a line: `rem` or `::`.
                let rest: String = chars[i..].iter().take(4).collect();
                chars[..i].iter().all(|c| c.is_whitespace())
                    && (rest.eq_ignore_ascii_case("rem ") || rest.starts_with("::"))
            } else {
                starts_with_at(&chars, i, lc)
            };
            // `#` inside a word (a colour, `C#`) is not a comment.
            let bounded = lc != "#" || i == 0 || !is_ident(chars[i - 1]);
            if hit && bounded {
                out[i..].fill(Tok::Comment);
                break;
            }
        }
        if let Some((open, _)) = lang.block_comment
            && starts_with_at(&chars, i, open)
        {
            let len = open.chars().count();
            out[i..i + len].fill(Tok::Comment);
            i += len;
            in_block = true;
            continue;
        }
        if lang.quotes.contains(&c) {
            // In Rust a lone `'` is a lifetime, not the start of a string.
            let lifetime = lang.name == "Rust"
                && c == '\''
                && !(chars.get(i + 1) == Some(&'\\') || chars.get(i + 2) == Some(&'\''));
            if !lifetime {
                let start = i;
                i += 1;
                while i < n && chars[i] != c {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i = (i + 1).min(n);
                out[start..i].fill(Tok::Str);
                continue;
            }
        }
        if c.is_ascii_digit() && (i == 0 || !is_ident(chars[i - 1])) {
            let start = i;
            while i < n && (is_ident(chars[i]) || chars[i] == '.') {
                i += 1;
            }
            out[start..i].fill(Tok::Number);
            continue;
        }
        if is_ident(c) {
            let start = i;
            while i < n && is_ident(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let keyword = if lang.fold_case {
                let w = word.to_lowercase();
                lang.keywords.contains(&w.as_str())
            } else {
                lang.keywords.contains(&word.as_str())
            };
            let next = chars[i..].iter().find(|c| !c.is_whitespace());
            let tok = if keyword {
                Tok::Keyword
            } else if next == Some(&'(') || (lang.name == "Rust" && next == Some(&'!')) {
                Tok::Call
            } else if c.is_uppercase() && lang.name != "Text" {
                Tok::Type
            } else {
                Tok::Plain
            };
            out[start..i].fill(tok);
            continue;
        }
        i += 1;
    }
    (out, in_block)
}

/// Whether a block comment is open at the start of `upto`, found by running
/// the lines above it. Far down a huge file the answer is taken as no rather
/// than paid for on every frame.
pub fn block_open_before(lines: &[String], upto: usize, lang: &Lang) -> bool {
    if lang.block_comment.is_none() || upto > 50_000 {
        return false;
    }
    let mut open = false;
    for l in &lines[..upto.min(lines.len())] {
        open = highlight(l, lang, open).1;
    }
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(line: &str, ext: &str) -> Vec<Tok> {
        highlight(line, detect(Path::new(&format!("x.{ext}"))), false).0
    }

    #[test]
    fn keywords_strings_and_comments() {
        let t = toks(r#"let s = "a\"b"; // hi"#, "rs");
        assert_eq!(t[0], Tok::Keyword);
        assert_eq!(t[8], Tok::Str);
        assert_eq!(t[13], Tok::Str);
        assert_eq!(t[14], Tok::Plain);
        assert_eq!(*t.last().unwrap(), Tok::Comment);
    }

    #[test]
    fn a_rust_lifetime_is_not_a_string() {
        let t = toks("fn f<'a>(x: &'a str) -> char { 'x' }", "rs");
        assert_eq!(t[5], Tok::Plain);
        assert_eq!(t[32], Tok::Str);
    }

    #[test]
    fn a_block_comment_runs_across_lines() {
        let lang = detect(Path::new("x.c"));
        let (_, open) = highlight("int a; /* start", lang, false);
        assert!(open);
        let (t, open) = highlight("still */ int b;", lang, true);
        assert!(!open);
        assert_eq!(t[0], Tok::Comment);
        assert_eq!(t[9], Tok::Keyword);
    }

    #[test]
    fn calls_types_and_numbers() {
        let t = toks("x = Foo.bar(42)", "py");
        assert_eq!(t[4], Tok::Type);
        assert_eq!(t[8], Tok::Call);
        assert_eq!(t[12], Tok::Number);
    }
}
