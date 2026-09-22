//! The tiny boolean language used by `depends_on`.
//!
//! `a`, `!a`, `a && b`, `a || b` and parentheses, over bool option keys.
//! `&&` binds tighter than `||`, as in C and Kconfig.

use std::fmt;

/// A parsed `depends_on` expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// The value of a bool option.
    Key(String),
    /// Logical not.
    Not(Box<Expr>),
    /// Both sides true.
    And(Box<Expr>, Box<Expr>),
    /// Either side true.
    Or(Box<Expr>, Box<Expr>),
}

/// Why an expression could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("in `{source_text}`: {reason}")]
pub struct ParseError {
    /// The expression as written.
    pub source_text: String,
    /// What was wrong with it.
    pub reason: String,
}

impl Expr {
    /// Parses an expression such as `a.b && !(c.d || e.f)`.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let tokens = tokenize(text).map_err(|reason| ParseError {
            source_text: text.to_string(),
            reason,
        })?;
        let mut parser = Parser {
            tokens,
            position: 0,
        };
        let expr = parser.or().map_err(|reason| ParseError {
            source_text: text.to_string(),
            reason,
        })?;
        if parser.position != parser.tokens.len() {
            return Err(ParseError {
                source_text: text.to_string(),
                reason: format!("unexpected `{}`", parser.tokens[parser.position]),
            });
        }
        Ok(expr)
    }

    /// Evaluates the expression, looking bool values up through `lookup`.
    pub fn eval(&self, lookup: &impl Fn(&str) -> bool) -> bool {
        match self {
            Expr::Key(key) => lookup(key),
            Expr::Not(inner) => !inner.eval(lookup),
            Expr::And(left, right) => left.eval(lookup) && right.eval(lookup),
            Expr::Or(left, right) => left.eval(lookup) || right.eval(lookup),
        }
    }

    /// Every key the expression mentions, for validating the schema.
    pub fn keys(&self) -> Vec<&str> {
        let mut keys = Vec::new();
        self.collect_keys(&mut keys);
        keys
    }

    fn collect_keys<'a>(&'a self, keys: &mut Vec<&'a str>) {
        match self {
            Expr::Key(key) => keys.push(key),
            Expr::Not(inner) => inner.collect_keys(keys),
            Expr::And(left, right) | Expr::Or(left, right) => {
                left.collect_keys(keys);
                right.collect_keys(keys);
            }
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Key(key) => write!(f, "{key}"),
            Expr::Not(inner) => write!(f, "!{inner}"),
            Expr::And(left, right) => write!(f, "({left} && {right})"),
            Expr::Or(left, right) => write!(f, "({left} || {right})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Key(String),
    Not,
    And,
    Or,
    Open,
    Close,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Key(key) => write!(f, "{key}"),
            Token::Not => write!(f, "!"),
            Token::And => write!(f, "&&"),
            Token::Or => write!(f, "||"),
            Token::Open => write!(f, "("),
            Token::Close => write!(f, ")"),
        }
    }
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&ch) = chars.peek() {
        match ch {
            ' ' | '\t' => {
                chars.next();
            }
            '!' => {
                chars.next();
                tokens.push(Token::Not);
            }
            '(' => {
                chars.next();
                tokens.push(Token::Open);
            }
            ')' => {
                chars.next();
                tokens.push(Token::Close);
            }
            '&' | '|' => {
                chars.next();
                if chars.next() != Some(ch) {
                    return Err(format!("`{ch}` must be doubled, as `{ch}{ch}`"));
                }
                tokens.push(if ch == '&' { Token::And } else { Token::Or });
            }
            ch if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' => {
                let mut key = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                        key.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Key(key));
            }
            other => return Err(format!("unexpected character `{other}`")),
        }
    }
    if tokens.is_empty() {
        return Err("the expression is empty".into());
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn or(&mut self) -> Result<Expr, String> {
        let mut left = self.and()?;
        while self.peek() == Some(&Token::Or) {
            self.position += 1;
            let right = self.and()?;
            left = Expr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        while self.peek() == Some(&Token::And) {
            self.position += 1;
            let right = self.unary()?;
            left = Expr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, String> {
        match self.tokens.get(self.position).cloned() {
            Some(Token::Not) => {
                self.position += 1;
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Some(Token::Open) => {
                self.position += 1;
                let inner = self.or()?;
                if self.peek() != Some(&Token::Close) {
                    return Err("a `(` is never closed".into());
                }
                self.position += 1;
                Ok(inner)
            }
            Some(Token::Key(key)) => {
                self.position += 1;
                Ok(Expr::Key(key))
            }
            Some(other) => Err(format!("expected an option key, found `{other}`")),
            None => Err("the expression ends too early".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(text: &str, on: &[&str]) -> bool {
        Expr::parse(text)
            .unwrap()
            .eval(&|key: &str| on.contains(&key))
    }

    #[test]
    fn a_single_key_is_its_own_value() {
        assert!(eval("a.b", &["a.b"]));
        assert!(!eval("a.b", &[]));
    }

    #[test]
    fn not_and_or_behave() {
        assert!(eval("!a", &[]));
        assert!(eval("a && b", &["a", "b"]));
        assert!(!eval("a && b", &["a"]));
        assert!(eval("a || b", &["b"]));
        assert!(!eval("a || b", &[]));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // a || (b && c), not (a || b) && c
        assert!(eval("a || b && c", &["a"]));
        assert_eq!(
            Expr::parse("a || b && c").unwrap().to_string(),
            "(a || (b && c))"
        );
    }

    #[test]
    fn parentheses_and_nested_not() {
        assert!(eval("!(a || b)", &[]));
        assert!(!eval("!(a || b)", &["b"]));
        assert!(eval("!!a", &["a"]));
    }

    #[test]
    fn keys_lists_every_mentioned_option() {
        let expr = Expr::parse("x.a && !(x.b || x.c)").unwrap();
        assert_eq!(expr.keys(), vec!["x.a", "x.b", "x.c"]);
    }

    #[test]
    fn malformed_expressions_explain_themselves() {
        let cases = [
            ("", "empty"),
            ("a &", "doubled"),
            ("a ||", "ends too early"),
            ("(a", "never closed"),
            ("a b", "unexpected `b`"),
            ("a == b", "unexpected character"),
        ];
        for (text, fragment) in cases {
            let err = Expr::parse(text).unwrap_err();
            assert!(
                err.to_string().contains(fragment),
                "`{text}` gave `{err}`, expected it to mention `{fragment}`"
            );
        }
    }
}
