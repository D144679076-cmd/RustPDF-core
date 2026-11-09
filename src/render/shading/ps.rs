//! PostScript-calculator shading functions (ISO 32000-1 §7.10.4, FunctionType 4).
//!
//! Minimal postfix interpreter over the operator subset real shading
//! functions use: numeric literals, arithmetic, stack manipulation, common
//! math, comparisons + logical operators, and `if`/`ifelse` with brace blocks.
//!
//! Pure Rust, dependency-free (R2); no panics (R1); errors are internal — the
//! caller attaches object context.

// ---------------------------------------------------------------------------
// Tokenisation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Operator(String),
    OpenBrace,
    CloseBrace,
}

fn tokenize(src: &[u8]) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b == b'%' {
            while i < src.len() && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'{' || b == b'}' {
            out.push(if b == b'{' {
                Token::OpenBrace
            } else {
                Token::CloseBrace
            });
            i += 1;
            continue;
        }
        let is_number_start = b.is_ascii_digit()
            || (b == b'.')
            || ((b == b'-' || b == b'+') && i + 1 < src.len() && src[i + 1].is_ascii_digit());
        if is_number_start {
            let start = i;
            i += 1;
            while i < src.len()
                && (src[i].is_ascii_digit()
                    || src[i] == b'.'
                    || src[i] == b'e'
                    || src[i] == b'E'
                    || ((src[i] == b'-' || src[i] == b'+') && ((src[i - 1] | 0x20) == b'e')))
            {
                i += 1;
            }
            let s = std::str::from_utf8(&src[start..i]).map_err(|_| "bad number literal")?;
            let n: f64 = s.parse().map_err(|_| format!("bad number `{}`", s))?;
            out.push(Token::Number(n));
            continue;
        }
        if b.is_ascii_alphabetic() || b == b'@' {
            let start = i;
            while i < src.len() && !src[i].is_ascii_whitespace() {
                i += 1;
            }
            let s = String::from_utf8_lossy(&src[start..i]).to_string();
            out.push(Token::Operator(s));
            continue;
        }
        return Err(format!("unexpected byte `{}` at {}", b as char, i));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Value {
    Num(f64),
    Block(Vec<Token>),
}

fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Evaluate a FunctionType 4 function with two inputs `(t0, t1)` — the entry
/// point required by function-based shading (ShadingType 1) and mesh
/// /Function overrides. `domain` is the function's /Domain in
/// `[x_lo, x_hi, y_lo, y_hi]` form (§7.10.4: inputs are normalized into it).
pub fn eval2(src: &[u8], t0: f64, t1: f64, domain: [f64; 4]) -> Result<Vec<f64>, String> {
    let a = norm01(t0, domain[0], domain[1]);
    let b = norm01(t1, domain[2], domain[3]);

    let tokens = tokenize(src)?;
    let mut stack = vec![Value::Num(a), Value::Num(b)];
    let mut i = 0usize;
    eval_tokens(&tokens, &mut i, &mut stack)?;
    Ok(stack.into_iter().filter_map(|v| v.as_num()).collect())
}

fn norm01(t: f64, lo: f64, hi: f64) -> f64 {
    if hi > lo {
        clamp01((t - lo) / (hi - lo))
    } else {
        0.0
    }
}

impl Value {
    fn as_num(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }
}

fn eval_tokens(tokens: &[Token], i: &mut usize, stack: &mut Vec<Value>) -> Result<(), String> {
    while *i < tokens.len() {
        let tok = &tokens[*i];
        *i += 1;
        match tok {
            Token::Number(n) => stack.push(Value::Num(*n)),
            Token::Operator(op) => apply_operator(op, stack)?,
            Token::OpenBrace => {
                // Scan to the matching close brace (nest-aware) and push the
                // enclosed tokens as an inert block value.
                let start = *i;
                let mut depth = 1usize;
                while *i < tokens.len() && depth > 0 {
                    match &tokens[*i] {
                        Token::OpenBrace => depth += 1,
                        Token::CloseBrace => depth -= 1,
                        _ => {}
                    }
                    *i += 1;
                }
                if depth != 0 {
                    return Err("unterminated `{` block".to_string());
                }
                stack.push(Value::Block(tokens[start..*i - 1].to_vec()));
            }
            Token::CloseBrace => return Err("unmatched `}".to_string()),
        }
    }
    Ok(())
}

fn pop_num(stack: &mut Vec<Value>) -> Result<f64, String> {
    match stack.pop() {
        Some(Value::Num(n)) => Ok(n),
        _ => Err("stack underflow (expected number)".to_string()),
    }
}

fn pop_block(stack: &mut Vec<Value>) -> Result<Vec<Token>, String> {
    match stack.pop() {
        Some(Value::Block(b)) => Ok(b),
        _ => Err("expected a `{...}` block".to_string()),
    }
}

fn apply_operator(op: &str, stack: &mut Vec<Value>) -> Result<(), String> {
    macro_rules! bin {
        ($f:expr) => {{
            if stack.len() < 2 {
                return Err(format!("stack underflow for `{}`", op));
            }
            let b = pop_num(stack)?;
            let a = pop_num(stack)?;
            stack.push(Value::Num($f(a, b)));
        }};
    }
    macro_rules! un {
        ($f:expr) => {{
            let a = pop_num(stack)?;
            stack.push(Value::Num($f(a)));
        }};
    }
    match op {
        "dup" => {
            let v = stack.last().ok_or("stack underflow for `dup`")?.clone();
            stack.push(v);
        }
        "pop" => {
            if stack.pop().is_none() {
                return Err("stack underflow for `pop`".to_string());
            }
        }
        "exch" => {
            if stack.len() < 2 {
                return Err("stack underflow for `exch`".to_string());
            }
            let n = stack.len();
            stack.swap(n - 1, n - 2);
        }
        "index" => {
            let k = pop_num(stack)? as i64;
            if k < 0 || (k as usize) >= stack.len() {
                return Err("`index` out of range".to_string());
            }
            let v = stack[stack.len() - 1 - k as usize].clone();
            stack.push(v);
        }
        "copy" => {
            let n = pop_num(stack)? as usize;
            if n > stack.len() || n == 0 {
                return Err("`copy` out of range".to_string());
            }
            let start = stack.len() - n;
            let copied: Vec<Value> = stack[start..].to_vec();
            stack.extend(copied);
        }
        "add" => bin!(|a: f64, b| a + b),
        "sub" => bin!(|a: f64, b| a - b),
        "mul" => bin!(|a: f64, b| a * b),
        "div" => bin!(|a: f64, b| if b == 0.0 { 0.0 } else { a / b }),
        "mod" => bin!(|a: f64, b| if b == 0.0 { 0.0 } else { a % b }),
        "min" => bin!(|a: f64, b| a.min(b)),
        "max" => bin!(|a: f64, b| a.max(b)),
        "and" => bin!(|a: f64, b| ((a != 0.0) && (b != 0.0)) as i32 as f64),
        "or" => bin!(|a: f64, b| ((a != 0.0) || (b != 0.0)) as i32 as f64),
        "xor" => bin!(|a: f64, b| ((a != 0.0) ^ (b != 0.0)) as i32 as f64),
        "gt" => bin!(|a: f64, b| (a > b) as i32 as f64),
        "lt" => bin!(|a: f64, b| (a < b) as i32 as f64),
        "ge" => bin!(|a: f64, b| (a >= b) as i32 as f64),
        "le" => bin!(|a: f64, b| (a <= b) as i32 as f64),
        "eq" => bin!(|a: f64, b| (a == b) as i32 as f64),
        "ne" => bin!(|a: f64, b| (a != b) as i32 as f64),
        "neg" => un!(|a: f64| -a),
        "abs" => un!(|a: f64| a.abs()),
        "sqrt" => un!(|a: f64| a.sqrt()),
        "sin" => un!(|a: f64| a.sin()),
        "cos" => un!(|a: f64| a.cos()),
        "atan" => {
            // PS atan: num denom atan → angle in degrees.
            let denom = pop_num(stack)?;
            let num = pop_num(stack)?;
            stack.push(Value::Num((num / denom).atan().to_degrees()));
        }
        "exp" => un!(|a: f64| a.exp()),
        "ln" => un!(|a: f64| a.ln()),
        "log" => un!(|a: f64| a.log10()),
        "floor" => un!(|a: f64| a.floor()),
        "ceil" => un!(|a: f64| a.ceil()),
        "round" => un!(|a: f64| a.round()),
        "truncate" => un!(|a: f64| a.trunc()),
        "cvx" | "cvr" | "cvi" | "cvs" => {}
        "if" => {
            // PDF §7.10.4: `cond {expr} if` — proc sits on top of the boolean.
            let block = pop_block(stack)?;
            let cond = pop_num(stack)? != 0.0;
            if cond {
                eval_tokens(&block, &mut 0, stack)?;
            }
        }
        "ifelse" => {
            // PDF §7.10.4: `cond {true} {false} ifelse` — false-proc on top.
            let else_block = pop_block(stack)?;
            let if_block = pop_block(stack)?;
            let cond = pop_num(stack)? != 0.0;
            if cond {
                eval_tokens(&if_block, &mut 0, stack)?;
            } else {
                eval_tokens(&else_block, &mut 0, stack)?;
            }
        }
        other => {
            // Unknown operator: fail loudly rather than silently mis-color.
            return Err(format!("unsupported operator `{}`", other));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests (R5: happy + error paths)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &[u8]) -> Vec<f64> {
        eval2(src, 0.25, 0.75, [0.0, 1.0, 0.0, 1.0]).unwrap()
    }

    #[test]
    fn arithmetic_basic() {
        assert_eq!(run(b"12 3 add"), vec![0.25, 0.75, 15.0]);
        assert_eq!(run(b"12 3 sub"), vec![0.25, 0.75, 9.0]);
        assert_eq!(run(b"2 3 mul"), vec![0.25, 0.75, 6.0]);
        assert_eq!(run(b"9 2 div"), vec![0.25, 0.75, 4.5]);
        assert_eq!(run(b"7 3 mod"), vec![0.25, 0.75, 1.0]);
    }

    #[test]
    fn stack_operations() {
        assert_eq!(run(b"5 dup mul"), vec![0.25, 0.75, 25.0]);
        assert_eq!(run(b"3 7 exch sub"), vec![0.25, 0.75, 4.0]);
        assert_eq!(
            run(b"1 2 3 2 copy"),
            vec![0.25, 0.75, 1.0, 2.0, 3.0, 2.0, 3.0]
        );
        assert_eq!(run(b"1 2 3 1 index"), vec![0.25, 0.75, 1.0, 2.0, 3.0, 2.0]);
    }

    #[test]
    fn math_and_comparisons() {
        assert_eq!(run(b"4 abs"), vec![0.25, 0.75, 4.0]);
        assert_eq!(run(b"-5 neg"), vec![0.25, 0.75, 5.0]);
        assert_eq!(run(b"9 sqrt"), vec![0.25, 0.75, 3.0]);
        assert_eq!(run(b"3 4 ge"), vec![0.25, 0.75, 0.0]);
        assert_eq!(run(b"3 4 le"), vec![0.25, 0.75, 1.0]);
        assert_eq!(run(b"3 4 min"), vec![0.25, 0.75, 3.0]);
        assert_eq!(run(b"3 4 max"), vec![0.25, 0.75, 4.0]);
    }

    #[test]
    fn if_and_ifelse() {
        assert_eq!(run(b"3 4 lt { 10 } { 20 } ifelse"), vec![0.25, 0.75, 10.0]);
        assert_eq!(run(b"3 4 gt { 10 } { 20 } ifelse"), vec![0.25, 0.75, 20.0]);
        // Nested brace groups tokenize and defer correctly.
        assert_eq!(run(b"1 { { 5 5 add } } if"), vec![0.25, 0.75]);
        assert_eq!(run(b"0 { } if 3"), vec![0.25, 0.75, 3.0]);
    }

    #[test]
    fn inputs_reach_the_stack() {
        // x (t0=0.25) down the bottom; y (t1=0.75) on top.
        assert_eq!(run(b"exch sub"), vec![0.5]);
        // Domain normalization: t0 outside domain clamps; dup copies top (t1).
        assert_eq!(
            eval2(b"dup", 5.0, 0.75, [0.0, 1.0, 0.0, 1.0]).unwrap(),
            vec![1.0, 0.75, 0.75]
        );
    }

    #[test]
    fn duplicate_returns_one_value() {
        assert_eq!(
            run(b"1 exch sub dup dup 3 mul"),
            vec![0.25, 0.25, 0.25, 0.75]
        );
    }

    #[test]
    fn error_on_unsupported_operator() {
        assert!(eval2(b"foobar", 0.0, 0.0, [0.0, 1.0, 0.0, 1.0]).is_err());
    }

    #[test]
    fn error_on_unmatched_braces() {
        assert!(eval2(b"{ 1 2", 0.0, 0.0, [0.0, 1.0, 0.0, 1.0]).is_err());
        assert!(eval2(b"1 } 2", 0.0, 0.0, [0.0, 1.0, 0.0, 1.0]).is_err());
    }

    #[test]
    fn error_on_stack_underflow() {
        // eval2 preloads 2 inputs, so a single `add` succeeds; three chain
        // past what is available.
        assert!(eval2(b"add add add", 0.0, 0.0, [0.0, 1.0, 0.0, 1.0]).is_err());
    }
}
