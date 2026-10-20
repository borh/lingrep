//! Recursive descent over the lexed clause.
//!
//! Conjunction is the default and disjunction requires explicit parentheses,
//! so `a & b | c` is refused rather than resolved by a precedence rule the
//! author cannot see.

use std::ops::Range;

use super::lex::{Lexed, Token, lex};
use super::{Comparison, Membership, Predicate, ScopeError};

#[derive(Clone, Debug)]
pub(crate) enum Clause {
    Within {
        kind: String,
        body: Box<Clause>,
        negated: bool,
    },
    All(Vec<Clause>),
    Any(Vec<Clause>),
    Leaf(Predicate),
}

pub(crate) fn parse(source: &str) -> Result<Clause, ScopeError> {
    let tokens = lex(source).map_err(|(message, byte_range)| ScopeError {
        message,
        byte_range,
    })?;
    if tokens.is_empty() {
        return Err(ScopeError {
            message: "a scope clause must contain at least one predicate".to_owned(),
            byte_range: 0..source.len(),
        });
    }
    let mut parser = Parser {
        tokens: &tokens,
        cursor: 0,
        source_len: source.len(),
    };
    if tokens.len() > 4096 {
        return Err(parser.error_here("scope clause exceeds 4096 tokens"));
    }
    let mut depth = 0usize;
    for token in &tokens {
        if token.token == Token::OpenParen {
            depth += 1;
        }
        if depth > 64 {
            return Err(parser.error_here("scope clause nesting exceeds 64 levels"));
        }
        if token.token == Token::CloseParen {
            depth = depth.saturating_sub(1);
        }
    }
    let clause = parser.clause(false)?;
    validate_owners(&clause, false).map_err(|message| ScopeError {
        message,
        byte_range: 0..source.len(),
    })?;
    parser.expect_end()?;
    Ok(clause)
}

fn validate_owners(clause: &Clause, division: bool) -> Result<(), String> {
    use super::Predicate;
    match clause {
        Clause::Within { body, .. } => {
            if division {
                return Err("nested within predicates are unsupported".into());
            }
            validate_owners(body, true)
        }
        Clause::All(parts) | Clause::Any(parts) => {
            for part in parts {
                validate_owners(part, division)?;
            }
            Ok(())
        }
        Clause::Leaf(Predicate::Section(..)) if division => {
            Err("@section is an active-label test outside within, not division metadata".into())
        }
        Clause::Leaf(Predicate::Section(..)) => Ok(()),
        Clause::Leaf(
            Predicate::Compare(key, ..)
            | Predicate::Order(key, ..)
            | Predicate::Member(key, ..)
            | Predicate::Contains(key, ..),
        ) => {
            if division && !key.starts_with('@') {
                return Ok(());
            }
            if !division
                && ["@document.", "@sentence."].iter().any(|prefix| {
                    key.strip_prefix(prefix)
                        .is_some_and(|name| !name.is_empty())
                })
            {
                return Ok(());
            }
            Err("metadata must name its owner: @document.KEY, @sentence.KEY, or a bare key inside within(KIND, ...)".into())
        }
    }
}

struct Parser<'a> {
    tokens: &'a [Lexed],
    cursor: usize,
    source_len: usize,
}

impl Parser<'_> {
    /// `nested` distinguishes the top level, where `|` is refused, from the
    /// inside of a parenthesised group, where it is the alternation operator.
    fn clause(&mut self, nested: bool) -> Result<Clause, ScopeError> {
        let mut all = vec![self.disjunct()?];
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.cursor += 1;
                    all.push(self.disjunct()?);
                }
                Some(Token::Or) if !nested => {
                    return Err(self.error_here(
                        "`|` is only available inside parentheses; write `(a | b)` to group an \
                         alternation",
                    ));
                }
                _ => break,
            }
        }
        Ok(if all.len() == 1 {
            all.remove(0)
        } else {
            Clause::All(all)
        })
    }

    fn disjunct(&mut self) -> Result<Clause, ScopeError> {
        if matches!(self.peek(), Some(Token::Within | Token::NotWithin)) {
            let negated = self.peek() == Some(&Token::NotWithin);
            self.cursor += 1;
            self.expect(&Token::OpenParen, "expected `(` after within")?;
            let kind = self.identifier()?;
            crate::corpus::divisions::validate_kind(&kind)
                .map_err(|error| self.error_here(&error.to_string()))?;
            self.expect(&Token::Comma, "expected `,` after division kind")?;
            let body = self.clause(true)?;
            self.expect(&Token::CloseParen, "expected `)` after division predicate")?;
            return Ok(Clause::Within {
                kind,
                body: Box::new(body),
                negated,
            });
        }
        if self.peek() == Some(&Token::OpenParen) {
            self.cursor += 1;
            let mut any = vec![self.clause(true)?];
            while self.peek() == Some(&Token::Or) {
                self.cursor += 1;
                any.push(self.clause(true)?);
            }
            self.expect(&Token::CloseParen, "expected `)`")?;
            return Ok(if any.len() == 1 {
                any.remove(0)
            } else {
                Clause::Any(any)
            });
        }
        Ok(Clause::Leaf(self.predicate()?))
    }

    fn predicate(&mut self) -> Result<Predicate, ScopeError> {
        if self.peek() == Some(&Token::Section) {
            return self.section_predicate();
        }
        let key = self.identifier()?;
        let Some((operator, operator_span)) = self.next_owned() else {
            return Err(self.error_here("expected an operator"));
        };
        Ok(match operator {
            Token::Eq => Predicate::Compare(key, Comparison::Equal, self.value()?),
            Token::Ne => Predicate::Compare(key, Comparison::NotEqual, self.value()?),
            Token::Lt => Predicate::Order(key, Comparison::Less, self.decimal()?),
            Token::Le => Predicate::Order(key, Comparison::LessEqual, self.decimal()?),
            Token::Gt => Predicate::Order(key, Comparison::Greater, self.decimal()?),
            Token::Ge => Predicate::Order(key, Comparison::GreaterEqual, self.decimal()?),
            Token::In => Predicate::Member(key, Membership::In, self.value_set()?),
            Token::NotIn => Predicate::Member(key, Membership::NotIn, self.value_set()?),
            Token::Contains => Predicate::Contains(key, false, self.value()?),
            Token::NotContains => Predicate::Contains(key, true, self.value()?),
            _ => {
                return Err(ScopeError {
                    message: "expected a comparison, `in`, or `contains`".to_owned(),
                    byte_range: operator_span,
                });
            }
        })
    }

    /// `@section` takes membership operators only.
    ///
    /// `=` and `!=` are normalised into single-member `In` and `NotIn`. They
    /// are exact complements here — a label set is total, so unlike a metadata
    /// value there is no third answer to preserve.
    fn section_predicate(&mut self) -> Result<Predicate, ScopeError> {
        let sigil = self.span_here();
        self.cursor += 1;
        let Some((operator, operator_span)) = self.next_owned() else {
            return Err(self.error_here("expected an operator"));
        };
        Ok(match operator {
            Token::Eq => Predicate::Section(Membership::In, vec![self.value()?]),
            Token::Ne => Predicate::Section(Membership::NotIn, vec![self.value()?]),
            Token::In => Predicate::Section(Membership::In, self.value_set()?),
            Token::NotIn => Predicate::Section(Membership::NotIn, self.value_set()?),
            _ => {
                return Err(ScopeError {
                    message: "`@section` takes `=`, `!=`, `in` or `not in`; a section label is \
                              an opaque string with no order and no parts"
                        .to_owned(),
                    byte_range: sigil.start..operator_span.end,
                });
            }
        })
    }

    fn identifier(&mut self) -> Result<String, ScopeError> {
        if let Some(Token::Identifier(key) | Token::Value(key)) = self.peek() {
            let key = key.clone();
            self.cursor += 1;
            return Ok(key);
        }
        Err(self.error_here("expected a metadata key"))
    }

    fn value(&mut self) -> Result<String, ScopeError> {
        if let Some(Token::Identifier(value) | Token::Value(value)) = self.peek() {
            let value = value.clone();
            self.cursor += 1;
            return Ok(value);
        }
        Err(self.error_here("expected a value"))
    }

    /// An ordered comparison's literal is authored text, so it is checked here
    /// rather than becoming a per-document shape mismatch.
    fn decimal(&mut self) -> Result<f64, ScopeError> {
        let span = self.span_here();
        let value = self.value()?;
        value.parse::<f64>().map_err(|_| ScopeError {
            message: format!("`{value}` is not a decimal; an ordered comparison needs one"),
            byte_range: span,
        })
    }

    fn value_set(&mut self) -> Result<Vec<String>, ScopeError> {
        self.expect(&Token::OpenBrace, "expected `{`")?;
        let mut members = vec![self.value()?];
        while self.peek() == Some(&Token::Comma) {
            self.cursor += 1;
            members.push(self.value()?);
        }
        self.expect(&Token::CloseBrace, "expected `}`")?;
        Ok(members)
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor).map(|lexed| &lexed.token)
    }

    fn next_owned(&mut self) -> Option<(Token, Range<usize>)> {
        let lexed = self.tokens.get(self.cursor)?;
        self.cursor += 1;
        Some((lexed.token.clone(), lexed.span.clone()))
    }

    fn expect(&mut self, expected: &Token, message: &str) -> Result<(), ScopeError> {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            return Ok(());
        }
        Err(self.error_here(message))
    }

    fn expect_end(&self) -> Result<(), ScopeError> {
        if self.cursor == self.tokens.len() {
            return Ok(());
        }
        Err(self.error_here("unexpected trailing input"))
    }

    fn span_here(&self) -> Range<usize> {
        self.tokens.get(self.cursor).map_or_else(
            || self.source_len..self.source_len,
            |lexed| lexed.span.clone(),
        )
    }

    fn error_here(&self, message: &str) -> ScopeError {
        ScopeError {
            message: message.to_owned(),
            byte_range: self.span_here(),
        }
    }
}
