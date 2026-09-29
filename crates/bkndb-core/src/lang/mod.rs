//! Text query languages layered over the builder APIs:
//!
//! - [`sql`] — a SQL subset for relational tables (`relational` feature);
//! - [`graph`] — `MATCH` patterns over the graph (`graph` feature).
//!
//! Both are parsed into ASTs that are executed through exactly the same
//! code paths as the builder APIs, so the planner (pk / index / scan choice),
//! constraints and transactions behave identically either way.
//!
//! Values are best passed as parameters — `?` (positional), `?2` / `$2`
//! (numbered, 1-based) or `:name` — which accept every value kind, bound
//! from [`Params`].
// Some shared parser helpers are used by only one of the two languages.
#![cfg_attr(not(all(feature = "graph", feature = "relational")), allow(dead_code))]

use std::collections::BTreeMap;

use crate::value::PropValue;
use crate::BknError;

#[cfg(feature = "graph")]
pub mod graph;
pub(crate) mod lexer;
#[cfg(feature = "relational")]
pub mod sql;
pub(crate) mod time;

use lexer::{err_at, Tok, Token};

/// Parameter values for a statement: positional (`?`, `?N`, `$N`) and/or
/// named (`:name`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Params {
    pub positional: Vec<PropValue>,
    pub named: BTreeMap<String, PropValue>,
}

impl Params {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn positional(values: impl IntoIterator<Item = impl Into<PropValue>>) -> Self {
        Self { positional: values.into_iter().map(Into::into).collect(), named: BTreeMap::new() }
    }

    pub fn named<K: Into<String>, V: Into<PropValue>>(values: impl IntoIterator<Item = (K, V)>) -> Self {
        Self { positional: Vec::new(), named: values.into_iter().map(|(k, v)| (k.into(), v.into())).collect() }
    }

    /// Adds a named parameter.
    pub fn with(mut self, name: impl Into<String>, value: impl Into<PropValue>) -> Self {
        self.named.insert(name.into(), value.into());
        self
    }
}

impl From<()> for Params {
    fn from(_: ()) -> Self {
        Params::none()
    }
}

impl From<Vec<PropValue>> for Params {
    fn from(v: Vec<PropValue>) -> Self {
        Params { positional: v, named: BTreeMap::new() }
    }
}

impl From<BTreeMap<String, PropValue>> for Params {
    fn from(named: BTreeMap<String, PropValue>) -> Self {
        Params { positional: Vec::new(), named }
    }
}

/// Tabular output of a statement: named columns and rows of values.
/// `affected` counts rows/entities written (0 for pure queries).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<PropValue>>,
    pub affected: u64,
}

/// A reference to a parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamRef {
    /// 0-based position.
    Index(usize),
    Name(String),
}

/// A value in a statement: a literal, a parameter, or a list/map built from
/// those. Bound to a concrete [`PropValue`] at execution time.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Lit(PropValue),
    Param(ParamRef),
    List(Vec<Operand>),
    Map(Vec<(String, Operand)>),
}

impl Operand {
    pub fn bind(&self, params: &Params) -> Result<PropValue, BknError> {
        Ok(match self {
            Operand::Lit(v) => v.clone(),
            Operand::Param(ParamRef::Index(i)) => params.positional.get(*i).cloned().ok_or_else(|| {
                BknError::InvalidQuery(format!(
                    "parameter {} is not bound ({} positional parameter(s) given)",
                    i + 1,
                    params.positional.len()
                ))
            })?,
            Operand::Param(ParamRef::Name(n)) => params
                .named
                .get(n)
                .cloned()
                .ok_or_else(|| BknError::InvalidQuery(format!("named parameter '{n}' is not bound")))?,
            Operand::List(items) => PropValue::List(items.iter().map(|o| o.bind(params)).collect::<Result<_, _>>()?),
            Operand::Map(entries) => {
                PropValue::Map(entries.iter().map(|(k, o)| Ok((k.clone(), o.bind(params)?))).collect::<Result<_, BknError>>()?)
            }
        })
    }
}

/// Token cursor with the helpers both languages' recursive-descent parsers
/// share.
pub(crate) struct Cursor<'s> {
    pub src: &'s str,
    toks: Vec<Token>,
    pos: usize,
    /// Next index for a bare `?`.
    next_positional: usize,
}

impl<'s> Cursor<'s> {
    pub fn new(src: &'s str, dialect: lexer::Dialect) -> Result<Self, BknError> {
        Ok(Self { src, toks: lexer::tokenize(src, dialect)?, pos: 0, next_positional: 0 })
    }

    pub fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    pub fn peek_at(&self, ahead: usize) -> Option<&Tok> {
        self.toks.get(self.pos + ahead).map(|t| &t.tok)
    }

    pub fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.tok.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// Byte offset of the current token in the source (its length at the end).
    pub fn current_pos(&self) -> usize {
        self.toks.get(self.pos).map_or(self.src.len(), |t| t.pos)
    }

    pub fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    /// An error pointing at the current token.
    pub fn err(&self, msg: impl std::fmt::Display) -> BknError {
        match self.toks.get(self.pos) {
            Some(t) => err_at(self.src, t.pos, msg),
            None => BknError::InvalidQuery(format!("{msg} (at end of input)")),
        }
    }

    pub fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(w)) if w.eq_ignore_ascii_case(kw))
    }

    pub fn is_kw_at(&self, ahead: usize, kw: &str) -> bool {
        matches!(self.peek_at(ahead), Some(Tok::Ident(w)) if w.eq_ignore_ascii_case(kw))
    }

    pub fn eat_kw(&mut self, kw: &str) -> bool {
        if self.is_kw(kw) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect_kw(&mut self, kw: &str) -> Result<(), BknError> {
        if self.eat_kw(kw) {
            Ok(())
        } else {
            Err(self.err(format!("expected {kw}")))
        }
    }

    pub fn is_sym(&self, sym: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(s)) if *s == sym)
    }

    pub fn eat_sym(&mut self, sym: &str) -> bool {
        if self.is_sym(sym) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    pub fn expect_sym(&mut self, sym: &str) -> Result<(), BknError> {
        if self.eat_sym(sym) {
            Ok(())
        } else {
            Err(self.err(format!("expected '{sym}'")))
        }
    }

    /// An identifier (quoted or not). `reserved` keywords are rejected
    /// unless quoted.
    pub fn ident(&mut self, what: &str, reserved: &[&str]) -> Result<String, BknError> {
        match self.peek() {
            Some(Tok::Ident(w)) if !reserved.iter().any(|r| w.eq_ignore_ascii_case(r)) => {
                let w = w.clone();
                self.pos += 1;
                Ok(w)
            }
            Some(Tok::QuotedIdent(w)) => {
                let w = w.clone();
                self.pos += 1;
                Ok(w)
            }
            _ => Err(self.err(format!("expected {what}"))),
        }
    }

    /// A possibly dotted name: `a`, `a.b.0`.
    pub fn path(&mut self, what: &str, reserved: &[&str]) -> Result<String, BknError> {
        let mut name = self.ident(what, reserved)?;
        while self.is_sym(".") {
            self.pos += 1;
            match self.next() {
                Some(Tok::Ident(w) | Tok::QuotedIdent(w)) => name = format!("{name}.{w}"),
                Some(Tok::Int(i)) if i >= 0 => name = format!("{name}.{i}"),
                _ => return Err(self.err("expected a field name or index after '.'")),
            }
        }
        Ok(name)
    }

    /// A non-negative integer literal.
    pub fn usize_lit(&mut self, what: &str) -> Result<usize, BknError> {
        match self.next() {
            Some(Tok::Int(i)) if i >= 0 => Ok(i as usize),
            _ => {
                self.pos = self.pos.saturating_sub(1);
                Err(self.err(format!("expected {what} (a non-negative integer)")))
            }
        }
    }

    /// Whether the next token starts an operand.
    pub fn at_operand(&self) -> bool {
        match self.peek() {
            Some(
                Tok::Int(_)
                | Tok::Float(_)
                | Tok::Str(_)
                | Tok::Bytes(_)
                | Tok::Param
                | Tok::NumberedParam(_)
                | Tok::NamedParam(_),
            ) => true,
            Some(Tok::Sym(s)) => matches!(*s, "-" | "[" | "{"),
            Some(Tok::Ident(w)) => ["NULL", "TRUE", "FALSE", "TIMESTAMP", "UUID"].iter().any(|k| w.eq_ignore_ascii_case(k)),
            _ => false,
        }
    }

    /// A literal, parameter, `[list]` or `{map: literal}`.
    pub fn operand(&mut self) -> Result<Operand, BknError> {
        let lit = |v: PropValue| Ok(Operand::Lit(v));
        match self.peek().cloned() {
            Some(Tok::Int(i)) => {
                self.pos += 1;
                lit(PropValue::Int(i))
            }
            Some(Tok::Float(f)) => {
                self.pos += 1;
                lit(PropValue::Float(f))
            }
            Some(Tok::Str(s)) => {
                self.pos += 1;
                lit(PropValue::Str(s))
            }
            Some(Tok::Bytes(b)) => {
                self.pos += 1;
                lit(PropValue::Bytes(b))
            }
            Some(Tok::Param) => {
                self.pos += 1;
                self.next_positional += 1;
                Ok(Operand::Param(ParamRef::Index(self.next_positional - 1)))
            }
            Some(Tok::NumberedParam(n)) => {
                self.pos += 1;
                Ok(Operand::Param(ParamRef::Index(n - 1)))
            }
            Some(Tok::NamedParam(n)) => {
                self.pos += 1;
                Ok(Operand::Param(ParamRef::Name(n)))
            }
            Some(Tok::Sym("-")) => {
                self.pos += 1;
                match self.next() {
                    Some(Tok::Int(i)) => lit(PropValue::Int(-i)),
                    Some(Tok::Float(f)) => lit(PropValue::Float(-f)),
                    _ => {
                        self.pos -= 1;
                        Err(self.err("expected a number after '-'"))
                    }
                }
            }
            Some(Tok::Sym("[")) => {
                self.pos += 1;
                let mut items = Vec::new();
                if !self.eat_sym("]") {
                    loop {
                        items.push(self.operand()?);
                        if self.eat_sym("]") {
                            break;
                        }
                        self.expect_sym(",")?;
                    }
                }
                Ok(Operand::List(items))
            }
            Some(Tok::Sym("{")) => {
                self.pos += 1;
                let mut entries = Vec::new();
                if !self.eat_sym("}") {
                    loop {
                        let key = match self.next() {
                            Some(Tok::Ident(k) | Tok::QuotedIdent(k) | Tok::Str(k)) => k,
                            _ => {
                                self.pos -= 1;
                                return Err(self.err("expected a map key"));
                            }
                        };
                        self.expect_sym(":")?;
                        entries.push((key, self.operand()?));
                        if self.eat_sym("}") {
                            break;
                        }
                        self.expect_sym(",")?;
                    }
                }
                Ok(Operand::Map(entries))
            }
            Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("NULL") => {
                self.pos += 1;
                lit(PropValue::Null)
            }
            Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("TRUE") || w.eq_ignore_ascii_case("FALSE") => {
                self.pos += 1;
                lit(PropValue::Bool(w.eq_ignore_ascii_case("TRUE")))
            }
            Some(Tok::Ident(w)) if w.eq_ignore_ascii_case("TIMESTAMP") || w.eq_ignore_ascii_case("UUID") => {
                self.pos += 1;
                let Some(Tok::Str(text)) = self.peek().cloned() else {
                    return Err(self.err(format!("expected a quoted {} after {w}", w.to_uppercase())));
                };
                let value = if w.eq_ignore_ascii_case("TIMESTAMP") {
                    time::parse_timestamp(&text).map(PropValue::Timestamp)
                } else {
                    time::parse_uuid(&text).map(PropValue::Uuid)
                };
                let v = value.ok_or_else(|| self.err(format!("invalid {} literal '{text}'", w.to_uppercase())))?;
                self.pos += 1;
                lit(v)
            }
            _ => Err(self.err("expected a value (literal or parameter)")),
        }
    }

    pub fn finish(&mut self) -> Result<(), BknError> {
        while self.eat_sym(";") {}
        if self.at_end() {
            Ok(())
        } else {
            Err(self.err("unexpected input after the end of the statement"))
        }
    }
}
