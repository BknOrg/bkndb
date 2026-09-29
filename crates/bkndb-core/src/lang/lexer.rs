//! Tokenizer shared by the SQL and graph-pattern languages.
use crate::BknError;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Tok {
    /// An identifier or keyword, as written. Keywords are matched
    /// case-insensitively by the parser.
    Ident(String),
    /// A `"double"` or `` `backtick` `` quoted identifier: never a keyword.
    QuotedIdent(String),
    Int(i64),
    Float(f64),
    Str(String),
    /// `x'00ff'`: a bytes literal.
    Bytes(Vec<u8>),
    /// `?` (positional, numbered left to right).
    Param,
    /// `?3`, `$3` (1-based) or `:name`.
    NumberedParam(usize),
    NamedParam(String),
    /// Punctuation / operators: `( ) [ ] { } , . ; * = == != <> < <= > >= - -> <- : |`.
    Sym(&'static str),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Token {
    pub tok: Tok,
    /// Byte offset in the source, for error messages.
    pub pos: usize,
}

pub(crate) fn err_at(src: &str, pos: usize, msg: impl std::fmt::Display) -> BknError {
    let line = src[..pos.min(src.len())].matches('\n').count() + 1;
    let col = src[..pos.min(src.len())].rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    BknError::InvalidQuery(format!("{msg} (at line {line}, column {col})"))
}

const SYMBOLS: [&str; 22] = [
    "->", "<-", "==", "!=", "<>", "<=", ">=", "(", ")", "[", "]", "{", "}", ",", ".", ";", "*", "=", "<", ">", "-", "|",
];

/// Which line-comment marker a dialect uses: SQL's `--`, or the graph
/// pattern language's `//` (where `--` is an undirected relationship).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    Sql,
    Graph,
}

pub(crate) fn tokenize(src: &str, dialect: Dialect) -> Result<Vec<Token>, BknError> {
    let line_comment = match dialect {
        Dialect::Sql => "--",
        Dialect::Graph => "//",
    };
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // Comments: a line comment to end of line, or `/* ... */`.
        if src[i..].starts_with(line_comment) {
            i = src[i..].find('\n').map_or(bytes.len(), |n| i + n);
            continue;
        }
        if src[i..].starts_with("/*") {
            i = src[i + 2..].find("*/").map(|n| i + 2 + n + 2).ok_or_else(|| err_at(src, start, "unterminated comment"))?;
            continue;
        }
        let tok = if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            if (c == b'x' || c == b'X') && bytes.get(i + 1) == Some(&b'\'') {
                let (s, end) = read_quoted(src, i + 1, b'\'')?;
                i = end;
                Tok::Bytes(decode_hex(&s).ok_or_else(|| err_at(src, start, "invalid hex in x'...' literal"))?)
            } else {
                let end = src[i..]
                    .char_indices()
                    .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_'))
                    .map_or(src.len(), |(n, _)| i + n);
                let word = src[i..end].to_string();
                i = end;
                Tok::Ident(word)
            }
        } else if c.is_ascii_digit() {
            let mut end = i;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let mut is_float = false;
            if end + 1 < bytes.len() && bytes[end] == b'.' && bytes[end + 1].is_ascii_digit() {
                is_float = true;
                end += 1;
                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    end += 1;
                }
            }
            if end < bytes.len() && (bytes[end] == b'e' || bytes[end] == b'E') {
                let mut e = end + 1;
                if e < bytes.len() && (bytes[e] == b'+' || bytes[e] == b'-') {
                    e += 1;
                }
                if e < bytes.len() && bytes[e].is_ascii_digit() {
                    is_float = true;
                    end = e;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                }
            }
            let text = &src[i..end];
            i = end;
            if is_float {
                Tok::Float(text.parse().map_err(|_| err_at(src, start, "invalid number"))?)
            } else {
                // i64::MIN's magnitude doesn't fit; the parser folds `-` into
                // the literal, so keep it as a float-free big-int error here.
                Tok::Int(text.parse().map_err(|_| err_at(src, start, format!("integer {text} is out of range")))?)
            }
        } else if c == b'\'' {
            let (s, end) = read_quoted(src, i, b'\'')?;
            i = end;
            Tok::Str(s)
        } else if c == b'"' || c == b'`' {
            let (s, end) = read_quoted(src, i, c)?;
            i = end;
            Tok::QuotedIdent(s)
        } else if c == b'$' && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_') {
            let end = src[i + 1..]
                .char_indices()
                .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_'))
                .map_or(src.len(), |(n, _)| i + 1 + n);
            let name = src[i + 1..end].to_string();
            i = end;
            Tok::NamedParam(name)
        } else if c == b'?' || c == b'$' {
            let mut end = i + 1;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            let digits = &src[i + 1..end];
            i = end;
            if digits.is_empty() {
                if c == b'$' {
                    return Err(err_at(src, start, "expected a parameter number after '$'"));
                }
                Tok::Param
            } else {
                let n: usize = digits.parse().map_err(|_| err_at(src, start, "parameter number out of range"))?;
                if n == 0 {
                    return Err(err_at(src, start, "parameters are numbered from 1"));
                }
                Tok::NumberedParam(n)
            }
        } else if c == b':'
            && dialect == Dialect::Sql
            && bytes.get(i + 1).is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            // `:name` parameters are SQL-only: in graph patterns `:` introduces
            // a label or relationship type.
            let end = src[i + 1..]
                .char_indices()
                .find(|&(_, ch)| !(ch.is_alphanumeric() || ch == '_'))
                .map_or(src.len(), |(n, _)| i + 1 + n);
            let name = src[i + 1..end].to_string();
            i = end;
            Tok::NamedParam(name)
        } else if c == b':' {
            i += 1;
            Tok::Sym(":")
        } else if let Some(sym) = SYMBOLS.iter().find(|s| src[i..].starts_with(**s)) {
            i += sym.len();
            Tok::Sym(sym)
        } else {
            let ch = src[i..].chars().next().unwrap_or('?');
            return Err(err_at(src, start, format!("unexpected character '{ch}'")));
        };
        out.push(Token { tok, pos: start });
    }
    Ok(out)
}

/// Reads a `q`-quoted run starting at `bytes[start] == q`; a doubled quote
/// inside stands for one quote. Returns the content and the index after it.
fn read_quoted(src: &str, start: usize, q: u8) -> Result<(String, usize), BknError> {
    let bytes = src.as_bytes();
    let mut out = String::new();
    let mut i = start + 1;
    let mut run = i;
    loop {
        match bytes.get(i) {
            None => return Err(err_at(src, start, "unterminated quoted text")),
            Some(&b) if b == q => {
                out.push_str(&src[run..i]);
                if bytes.get(i + 1) == Some(&q) {
                    out.push(q as char);
                    i += 2;
                    run = i;
                } else {
                    return Ok((out, i + 1));
                }
            }
            Some(_) => i += 1,
        }
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        tokenize(s, Dialect::Sql).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn tokenizes_sql() {
        assert_eq!(
            toks("SELECT a, \"b c\" FROM t WHERE x >= 1.5e2 AND y <> 'it''s' -- tail"),
            vec![
                Tok::Ident("SELECT".into()),
                Tok::Ident("a".into()),
                Tok::Sym(","),
                Tok::QuotedIdent("b c".into()),
                Tok::Ident("FROM".into()),
                Tok::Ident("t".into()),
                Tok::Ident("WHERE".into()),
                Tok::Ident("x".into()),
                Tok::Sym(">="),
                Tok::Float(150.0),
                Tok::Ident("AND".into()),
                Tok::Ident("y".into()),
                Tok::Sym("<>"),
                Tok::Str("it's".into()),
            ]
        );
    }

    #[test]
    fn tokenizes_params_bytes_and_arrows() {
        assert_eq!(
            toks("? ?2 $3 :name $other x'00ff'"),
            vec![
                Tok::Param,
                Tok::NumberedParam(2),
                Tok::NumberedParam(3),
                Tok::NamedParam("name".into()),
                Tok::NamedParam("other".into()),
                Tok::Bytes(vec![0, 255]),
            ]
        );
        let graph: Vec<Tok> = tokenize("(a:P)-[:R]->(b)<-", Dialect::Graph).unwrap().into_iter().map(|t| t.tok).collect();
        assert_eq!(
            graph,
            vec![
                Tok::Sym("("),
                Tok::Ident("a".into()),
                Tok::Sym(":"),
                Tok::Ident("P".into()),
                Tok::Sym(")"),
                Tok::Sym("-"),
                Tok::Sym("["),
                Tok::Sym(":"),
                Tok::Ident("R".into()),
                Tok::Sym("]"),
                Tok::Sym("->"),
                Tok::Sym("("),
                Tok::Ident("b".into()),
                Tok::Sym(")"),
                Tok::Sym("<-"),
            ]
        );
    }

    #[test]
    fn reports_positions() {
        let e = tokenize("SELECT\n  'open", Dialect::Sql).unwrap_err().to_string();
        assert!(e.contains("line 2, column 3"), "{e}");
        assert!(tokenize("a # b", Dialect::Sql).is_err());
        let graph: Vec<Tok> = tokenize("(a)--(b) // note", Dialect::Graph).unwrap().into_iter().map(|t| t.tok).collect();
        assert_eq!(graph.len(), 8, "{graph:?}");
    }
}
