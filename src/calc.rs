//! A small calculator for the launcher.
//!
//! Supports `+ - * / ^`, parentheses, unary minus, percentages (`15%`,
//! `15% of 80`), `mod`, factorial (`5!`), implicit multiplication (`2pi`,
//! `3(4+1)`), the constants `pi`, `tau` and `e`, and the functions `sqrt`,
//! `cbrt`, `abs`, `exp`, `ln`, `log` (base 10), `log2`, `sin`, `cos`, `tan`,
//! `asin`, `acos`, `atan`, `floor`, `ceil` and `round` (angles in radians).
//! `×`, `÷` and `x` (between numbers) also work.

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Op(char),
    Open,
    Close,
}

fn tokenize(input: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | ',' | '_' => i += 1,
            '0'..='9' | '.' => {
                let start = i;
                // Digits, a decimal point, and thousands separators (1,000).
                while i < chars.len()
                    && (chars[i].is_ascii_digit()
                        || chars[i] == '.'
                        || (chars[i] == ',' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit())))
                {
                    i += 1;
                }
                // Scientific notation: 1e3, 2.5e-4.
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '-' || chars[j] == '+') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        i = j;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let text: String = chars[start..i].iter().filter(|c| **c != ',').collect();
                tokens.push(Token::Num(text.parse().ok()?));
            }
            'a'..='z' | 'A'..='Z' | 'π' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == 'π') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
                // "x" between numbers means multiply.
                if word == "x" && matches!(tokens.last(), Some(Token::Num(_)) | Some(Token::Close)) {
                    tokens.push(Token::Op('*'));
                } else {
                    tokens.push(Token::Ident(word));
                }
            }
            '+' | '-' | '*' | '/' | '^' | '%' | '!' => {
                tokens.push(Token::Op(c));
                i += 1;
            }
            '×' | '·' => {
                tokens.push(Token::Op('*'));
                i += 1;
            }
            '÷' => {
                tokens.push(Token::Op('/'));
                i += 1;
            }
            '(' | '[' => {
                tokens.push(Token::Open);
                i += 1;
            }
            ')' | ']' => {
                tokens.push(Token::Close);
                i += 1;
            }
            _ => return None,
        }
    }
    Some(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn eat_op(&mut self, op: char) -> bool {
        if self.peek() == Some(&Token::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_ident(&mut self, word: &str) -> bool {
        if matches!(self.peek(), Some(Token::Ident(w)) if w == word) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    // expr := term (('+' | '-') term)*
    fn expr(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            if self.eat_op('+') {
                value += self.term()?;
            } else if self.eat_op('-') {
                value -= self.term()?;
            } else {
                return Some(value);
            }
        }
    }

    /// Whether the next token can start a factor (for implicit multiplication).
    fn starts_factor(&self) -> bool {
        matches!(self.peek(), Some(Token::Num(_)) | Some(Token::Open))
            || matches!(self.peek(), Some(Token::Ident(w)) if w != "mod" && w != "of")
    }

    // term := unary (('*' | '/' | 'mod' | 'of' | implicit) unary)*
    fn term(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            if self.eat_op('*') || self.eat_ident("of") {
                value *= self.unary()?;
            } else if self.eat_op('/') {
                value /= self.unary()?;
            } else if self.eat_ident("mod") {
                value = value.rem_euclid(self.unary()?);
            } else if self.starts_factor() {
                value *= self.unary()?;
            } else {
                return Some(value);
            }
        }
    }

    // power := postfix ('^' unary)?   (right-associative; binds tighter than unary minus)
    fn power(&mut self) -> Option<f64> {
        let base = self.postfix()?;
        if self.eat_op('^') {
            Some(base.powf(self.unary()?))
        } else {
            Some(base)
        }
    }

    // unary := ('-' | '+') unary | power
    fn unary(&mut self) -> Option<f64> {
        if self.eat_op('-') {
            Some(-self.unary()?)
        } else if self.eat_op('+') {
            self.unary()
        } else {
            self.power()
        }
    }

    // postfix := primary ('%' | '!')*
    fn postfix(&mut self) -> Option<f64> {
        let mut value = self.primary()?;
        loop {
            if self.eat_op('%') {
                value /= 100.0;
            } else if self.eat_op('!') {
                if value < 0.0 || value.fract() != 0.0 || value > 170.0 {
                    return None;
                }
                value = (1..=value as u64).map(|n| n as f64).product();
            } else {
                return Some(value);
            }
        }
    }

    fn primary(&mut self) -> Option<f64> {
        match self.next()? {
            Token::Num(n) => Some(n),
            Token::Open => {
                let value = self.expr()?;
                // Tolerate a missing closing parenthesis at the very end.
                if self.peek().is_some() && self.next()? != Token::Close {
                    return None;
                }
                Some(value)
            }
            Token::Ident(word) => match word.as_str() {
                "pi" | "π" => Some(std::f64::consts::PI),
                "tau" => Some(std::f64::consts::TAU),
                "e" => Some(std::f64::consts::E),
                name => {
                    let f: fn(f64) -> f64 = match name {
                        "sqrt" => f64::sqrt,
                        "cbrt" => f64::cbrt,
                        "abs" => f64::abs,
                        "exp" => f64::exp,
                        "ln" => f64::ln,
                        "log" => f64::log10,
                        "log2" => f64::log2,
                        "sin" => f64::sin,
                        "cos" => f64::cos,
                        "tan" => f64::tan,
                        "asin" => f64::asin,
                        "acos" => f64::acos,
                        "atan" => f64::atan,
                        "floor" => f64::floor,
                        "ceil" => f64::ceil,
                        "round" => f64::round,
                        _ => return None,
                    };
                    Some(f(self.power()?))
                }
            },
            _ => None,
        }
    }
}

/// Evaluates an expression, or `None` if it is not valid maths.
pub fn evaluate(input: &str) -> Option<f64> {
    // The parser is recursive: bound the input so a pasted wall of "(((("
    // cannot overflow the stack.
    if input.len() > 256 {
        return None;
    }
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return None;
    }
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.expr()?;
    (parser.pos >= parser.tokens.len() && value.is_finite()).then_some(value)
}

/// Whether the launcher should treat `query` as a calculation: it starts
/// with `=`, or it contains a digit and some actual maths (so that app names
/// like "7-Zip" or plain numbers do not trigger it).
pub fn looks_like_math(query: &str) -> bool {
    let q = query.trim();
    if let Some(rest) = q.strip_prefix('=') {
        return !rest.trim().is_empty();
    }
    let has_digit = q.chars().any(|c| c.is_ascii_digit());
    let has_op = q.chars().any(|c| "+*/^%!×÷(".contains(c))
        || q.contains(" - ")
        || q.chars().skip(1).any(|c| c == '-') && q.chars().all(|c| !c.is_alphabetic());
    let has_fn = ["sqrt", "sin", "cos", "tan", "log", "ln", "abs", "pi", " mod ", " of "].iter().any(|f| q.contains(f));
    has_digit && (has_op || has_fn)
}

/// Evaluates a launcher query (see [`looks_like_math`]).
pub fn evaluate_query(query: &str) -> Option<f64> {
    if !looks_like_math(query) {
        return None;
    }
    let q = query.trim();
    evaluate(q.strip_prefix('=').unwrap_or(q))
}

/// Formats a result: integers without decimals, others with up to 10
/// significant decimals and no trailing zeros.
pub fn format(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        return format!("{}", value as i64);
    }
    if value.abs() >= 1e15 || value.abs() < 1e-6 {
        return format!("{value:e}");
    }
    let s = format!("{value:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(s: &str) -> f64 {
        evaluate(s).unwrap_or_else(|| panic!("{s} did not evaluate"))
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval("2+3*4"), 14.0);
        assert_eq!(eval("(2+3)*4"), 20.0);
        assert_eq!(eval("10 - 4 - 3"), 3.0);
        assert_eq!(eval("2^3^2"), 512.0);
        assert_eq!(eval("-2^2"), -4.0);
        assert_eq!(eval("7 / 2"), 3.5);
        assert_eq!(eval("1,000 + 1"), 1001.0);
        assert_eq!(eval("3 × 4 ÷ 2"), 6.0);
        assert_eq!(eval("3 x 4"), 12.0);
    }

    #[test]
    fn percent_factorial_mod_and_implicit_multiplication() {
        assert_eq!(eval("15% of 80"), 12.0);
        assert_eq!(eval("50%"), 0.5);
        assert_eq!(eval("5!"), 120.0);
        assert_eq!(eval("17 mod 5"), 2.0);
        assert_eq!(eval("2(3+4)"), 14.0);
        assert!((eval("2pi") - std::f64::consts::TAU).abs() < 1e-12);
    }

    #[test]
    fn functions_and_constants() {
        assert_eq!(eval("sqrt(16)"), 4.0);
        assert_eq!(eval("sqrt 16"), 4.0);
        assert_eq!(eval("log(1000)"), 3.0);
        assert!((eval("sin(pi/2)") - 1.0).abs() < 1e-12);
        assert_eq!(eval("abs(-3) + floor(2.7)"), 5.0);
        assert_eq!(eval("2.5e3"), 2500.0);
        assert_eq!(eval("(1+2"), 3.0);
    }

    #[test]
    fn rejects_non_maths() {
        assert_eq!(evaluate("hello"), None);
        assert_eq!(evaluate("2 +"), None);
        assert_eq!(evaluate("1/0"), None);
        assert_eq!(evaluate("sqrt(-1)"), None);
        assert_eq!(evaluate("3!!x"), None);
        assert_eq!(evaluate(&"(".repeat(10_000)), None);
        assert_eq!(evaluate(&"-".repeat(10_000)), None);
    }

    #[test]
    fn decides_when_a_query_is_maths() {
        assert!(looks_like_math("2+2"));
        assert!(looks_like_math("sqrt(2)"));
        assert!(looks_like_math("15% of 80"));
        assert!(looks_like_math("10 - 3"));
        assert!(looks_like_math("=42"));
        assert!(!looks_like_math("42"));
        assert!(!looks_like_math("7-Zip"));
        assert!(!looks_like_math("Notepad++"));
        assert!(!looks_like_math("calc"));
        assert_eq!(evaluate_query("Notepad++"), None);
        assert_eq!(evaluate_query("= 6*7"), Some(42.0));
    }

    #[test]
    fn formats_results() {
        assert_eq!(format(14.0), "14");
        assert_eq!(format(3.5), "3.5");
        assert_eq!(format(1.0 / 3.0), "0.3333333333");
        assert_eq!(format(-2.0), "-2");
        assert_eq!(format(1e20), "1e20");
    }
}
