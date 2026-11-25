//! The formula language every Graph mode shares: parse once, compile to a small stack program,
//! then run it for every pixel or sample.
//!
//! * Numbers, names, `+ - * / % ^`, comparisons (`< > <= >= == !=`, true = 1), brackets.
//! * A number or a closing bracket followed by a name or `(` multiplies: `2x`, `3(x+1)`, `2 sin(x)`.
//! * `π` and `θ` may be typed for `pi` and `theta`.
//! * Functions: see [`FUNCS`]. `rand`, `noise` and `fbm` read the document's seed, so a picture made
//!   from random numbers comes out the same every time (until the seed changes).
//!
//! Names are resolved by the caller ([`compile`] takes a lookup): the mode's own variables (`x`, `y`,
//! `t`, `theta`, `u`, `i`, `n`) and the document's parameters. Any other name is "free":
//! [`free_names`] lists them so the app can make a slider for each.

use std::fmt;

/// A built-in function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Func {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sinh,
    Cosh,
    Tanh,
    Sqrt,
    Abs,
    Exp,
    Ln,
    Log,
    Floor,
    Ceil,
    Round,
    Fract,
    Sign,
    Min,
    Max,
    Pow,
    Mod,
    Hypot,
    Step,
    Clamp,
    Mix,
    Smoothstep,
    If,
    Rand,
    Noise,
    Fbm,
}

/// The built-in functions: name, number of arguments, which.
pub const FUNCS: &[(&str, usize, Func)] = &[
    ("sin", 1, Func::Sin),
    ("cos", 1, Func::Cos),
    ("tan", 1, Func::Tan),
    ("asin", 1, Func::Asin),
    ("acos", 1, Func::Acos),
    ("atan", 1, Func::Atan),
    ("atan2", 2, Func::Atan2),
    ("sinh", 1, Func::Sinh),
    ("cosh", 1, Func::Cosh),
    ("tanh", 1, Func::Tanh),
    ("sqrt", 1, Func::Sqrt),
    ("abs", 1, Func::Abs),
    ("exp", 1, Func::Exp),
    ("ln", 1, Func::Ln),
    ("log", 1, Func::Log),
    ("floor", 1, Func::Floor),
    ("ceil", 1, Func::Ceil),
    ("round", 1, Func::Round),
    ("fract", 1, Func::Fract),
    ("sign", 1, Func::Sign),
    ("min", 2, Func::Min),
    ("max", 2, Func::Max),
    ("pow", 2, Func::Pow),
    ("mod", 2, Func::Mod),
    ("hypot", 2, Func::Hypot),
    ("step", 2, Func::Step),
    ("clamp", 3, Func::Clamp),
    ("mix", 3, Func::Mix),
    ("smoothstep", 3, Func::Smoothstep),
    ("if", 3, Func::If),
    ("rand", 1, Func::Rand),
    ("noise", 2, Func::Noise),
    ("fbm", 3, Func::Fbm),
];

/// Names that are numbers.
pub const CONSTS: &[(&str, f64)] = &[("pi", std::f64::consts::PI), ("tau", std::f64::consts::TAU), ("e", std::f64::consts::E)];

/// Deepest stack a program may need (checked when compiling).
const STACK: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct Error {
    /// Character position the problem was found at.
    pub at: usize,
    /// In English; the app translates it for showing.
    pub msg: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.msg)
    }
}

fn err<T>(at: usize, msg: impl Into<String>) -> Result<T, Error> {
    Err(Error { at, msg: msg.into() })
}

// ------------------------------------------------------------------------------ tokens

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Name(String),
    Op(&'static str),
    Open,
    Close,
    Comma,
}

fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, Error> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let start = i;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // An exponent: 1e3, 2.5e-2 (but "2e" alone is 2 times e).
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    i = j;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let s: String = chars[start..i].iter().collect();
            match s.parse::<f64>() {
                Ok(v) => out.push((Tok::Num(v), start)),
                Err(_) => return err(start, format!("Bad number {s}")),
            }
            continue;
        }
        if c == 'π' {
            out.push((Tok::Name("pi".into()), start));
            i += 1;
            continue;
        }
        if c == 'θ' {
            out.push((Tok::Name("theta".into()), start));
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push((Tok::Name(chars[start..i].iter().collect()), start));
            continue;
        }
        let two: String = chars[i..(i + 2).min(chars.len())].iter().collect();
        if let Some(op) = ["<=", ">=", "==", "!=", "**"].iter().find(|o| **o == two) {
            out.push((Tok::Op(if *op == "**" { "^" } else { op }), start));
            i += 2;
            continue;
        }
        let tok = match c {
            '+' => Tok::Op("+"),
            '-' | '−' => Tok::Op("-"),
            '*' | '×' | '·' => Tok::Op("*"),
            '/' | '÷' => Tok::Op("/"),
            '%' => Tok::Op("%"),
            '^' => Tok::Op("^"),
            '<' => Tok::Op("<"),
            '>' => Tok::Op(">"),
            '(' => Tok::Open,
            ')' => Tok::Close,
            ',' => Tok::Comma,
            _ => return err(start, format!("Unexpected {c}")),
        };
        out.push((tok, start));
        i += 1;
    }
    Ok(out)
}

// ------------------------------------------------------------------------------ syntax tree

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Num(f64),
    Name(String, usize),
    Neg(Box<Node>),
    Bin(&'static str, Box<Node>, Box<Node>),
    Call(String, Vec<Node>, usize),
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    end: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }

    fn at(&self) -> usize {
        self.toks.get(self.pos).map_or(self.end, |t| t.1)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.0.clone());
        self.pos += 1;
        t
    }

    fn eat_op(&mut self, ops: &[&'static str]) -> Option<&'static str> {
        match self.peek() {
            Some(Tok::Op(o)) if ops.contains(o) => {
                let o = *o;
                self.pos += 1;
                Some(o)
            }
            _ => None,
        }
    }

    /// compare := sum [ (< > <= >= == !=) sum ]
    fn compare(&mut self) -> Result<Node, Error> {
        let a = self.sum()?;
        if let Some(op) = self.eat_op(&["<", ">", "<=", ">=", "==", "!="]) {
            let b = self.sum()?;
            return Ok(Node::Bin(op, Box::new(a), Box::new(b)));
        }
        Ok(a)
    }

    /// sum := product { (+ -) product }
    fn sum(&mut self) -> Result<Node, Error> {
        let mut a = self.product()?;
        while let Some(op) = self.eat_op(&["+", "-"]) {
            let b = self.product()?;
            a = Node::Bin(op, Box::new(a), Box::new(b));
        }
        Ok(a)
    }

    /// product := unary { (* / %) unary | unary }   (two things side by side multiply)
    fn product(&mut self) -> Result<Node, Error> {
        let mut a = self.unary()?;
        loop {
            let op = match self.eat_op(&["*", "/", "%"]) {
                Some(op) => op,
                None if matches!(self.peek(), Some(Tok::Num(_) | Tok::Name(_) | Tok::Open)) => "*",
                None => break,
            };
            let b = self.unary()?;
            a = Node::Bin(op, Box::new(a), Box::new(b));
        }
        Ok(a)
    }

    /// unary := - unary | + unary | power
    fn unary(&mut self) -> Result<Node, Error> {
        if self.eat_op(&["-"]).is_some() {
            return Ok(Node::Neg(Box::new(self.unary()?)));
        }
        if self.eat_op(&["+"]).is_some() {
            return self.unary();
        }
        self.power()
    }

    /// power := atom [ ^ unary ]   (right to left: 2^3^2 = 2^9, and -x^2 = -(x^2))
    fn power(&mut self) -> Result<Node, Error> {
        let a = self.atom()?;
        if self.eat_op(&["^"]).is_some() {
            let b = self.unary()?;
            return Ok(Node::Bin("^", Box::new(a), Box::new(b)));
        }
        Ok(a)
    }

    fn atom(&mut self) -> Result<Node, Error> {
        let at = self.at();
        match self.next() {
            Some(Tok::Num(v)) => Ok(Node::Num(v)),
            Some(Tok::Name(n)) => {
                if self.peek() == Some(&Tok::Open) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&Tok::Close) {
                        self.pos += 1;
                    } else {
                        loop {
                            args.push(self.compare()?);
                            match self.next() {
                                Some(Tok::Comma) => continue,
                                Some(Tok::Close) => break,
                                _ => return err(self.toks.get(self.pos - 1).map_or(self.end, |t| t.1), "Expected )"),
                            }
                        }
                    }
                    Ok(Node::Call(n, args, at))
                } else {
                    Ok(Node::Name(n, at))
                }
            }
            Some(Tok::Open) => {
                let e = self.compare()?;
                match self.next() {
                    Some(Tok::Close) => Ok(e),
                    _ => err(self.at().min(self.end), "Expected )"),
                }
            }
            None => err(at, "Formula ends too soon"),
            Some(t) => err(at, format!("Unexpected {}", tok_text(&t))),
        }
    }
}

fn tok_text(t: &Tok) -> String {
    match t {
        Tok::Num(v) => v.to_string(),
        Tok::Name(n) => n.clone(),
        Tok::Op(o) => o.to_string(),
        Tok::Open => "(".into(),
        Tok::Close => ")".into(),
        Tok::Comma => ",".into(),
    }
}

/// Parse a formula into its tree.
pub fn parse(src: &str) -> Result<Node, Error> {
    let toks = tokenize(src)?;
    let end = src.chars().count();
    if toks.is_empty() {
        return err(0, "Empty formula");
    }
    let mut p = Parser { toks, pos: 0, end };
    let n = p.compare()?;
    if p.pos < p.toks.len() {
        let at = p.at();
        let t = p.toks[p.pos].0.clone();
        return err(at, format!("Unexpected {}", tok_text(&t)));
    }
    Ok(n)
}

/// The names a formula uses that are neither functions nor constants, in order of first use.
pub fn names(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::Num(_) => {}
        Node::Name(n, _) => {
            if !CONSTS.iter().any(|c| c.0 == n) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        Node::Neg(a) => names(a, out),
        Node::Bin(_, a, b) => {
            names(a, out);
            names(b, out);
        }
        Node::Call(_, args, _) => args.iter().for_each(|a| names(a, out)),
    }
}

/// Names in `src` that `known` does not know: the ones that want a slider. A formula that does not
/// parse has none.
pub fn free_names(src: &str, known: impl Fn(&str) -> bool) -> Vec<String> {
    let Ok(tree) = parse(src) else { return Vec::new() };
    let mut all = Vec::new();
    names(&tree, &mut all);
    all.retain(|n| !known(n));
    all
}

// ------------------------------------------------------------------------------ program

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Num(f64),
    Var(u16),
    Neg,
    Bin(BinOp),
    Call(Func, u8),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
}

/// A compiled formula: run it with [`Prog::eval`].
#[derive(Clone, Debug, PartialEq)]
pub struct Prog {
    ops: Vec<Op>,
}

/// Compile `src`; `slot` gives the variable slot of a name (`None` = unknown name).
pub fn compile(src: &str, slot: &dyn Fn(&str) -> Option<usize>) -> Result<Prog, Error> {
    let tree = parse(src)?;
    let mut ops = Vec::new();
    emit(&tree, slot, &mut ops)?;
    // How deep the stack gets.
    let mut depth: i32 = 0;
    let mut most = 0;
    for op in &ops {
        depth += match op {
            Op::Num(_) | Op::Var(_) => 1,
            Op::Neg => 0,
            Op::Bin(_) => -1,
            Op::Call(_, n) => 1 - *n as i32,
        };
        most = most.max(depth);
    }
    if most as usize > STACK {
        return err(0, "Formula is too deeply nested");
    }
    Ok(Prog { ops })
}

fn emit(n: &Node, slot: &dyn Fn(&str) -> Option<usize>, ops: &mut Vec<Op>) -> Result<(), Error> {
    match n {
        Node::Num(v) => ops.push(Op::Num(*v)),
        Node::Name(name, at) => {
            if let Some(c) = CONSTS.iter().find(|c| c.0 == name) {
                ops.push(Op::Num(c.1));
            } else if let Some(s) = slot(name) {
                ops.push(Op::Var(s as u16));
            } else if FUNCS.iter().any(|f| f.0 == name) {
                return err(*at, format!("{name} needs ( )"));
            } else {
                return err(*at, format!("Unknown name {name}"));
            }
        }
        Node::Neg(a) => {
            emit(a, slot, ops)?;
            ops.push(Op::Neg);
        }
        Node::Bin(op, a, b) => {
            emit(a, slot, ops)?;
            emit(b, slot, ops)?;
            ops.push(Op::Bin(match *op {
                "+" => BinOp::Add,
                "-" => BinOp::Sub,
                "*" => BinOp::Mul,
                "/" => BinOp::Div,
                "%" => BinOp::Rem,
                "^" => BinOp::Pow,
                "<" => BinOp::Lt,
                ">" => BinOp::Gt,
                "<=" => BinOp::Le,
                ">=" => BinOp::Ge,
                "==" => BinOp::Eq,
                _ => BinOp::Ne,
            }));
        }
        Node::Call(name, args, at) => {
            let Some(i) = FUNCS.iter().position(|f| f.0 == name) else { return err(*at, format!("Unknown function {name}")) };
            if FUNCS[i].1 != args.len() {
                return err(*at, format!("Wrong number of values for {name}"));
            }
            for a in args {
                emit(a, slot, ops)?;
            }
            ops.push(Op::Call(FUNCS[i].2, FUNCS[i].1 as u8));
        }
    }
    Ok(())
}

impl Prog {
    /// A program that is always `v`.
    pub fn constant(v: f64) -> Prog {
        Prog { ops: vec![Op::Num(v)] }
    }

    /// Run with the variable slots `vars` and the random seed `seed`. A bad slot reads as 0.
    pub fn eval(&self, vars: &[f64], seed: u32) -> f64 {
        let mut st = [0.0f64; STACK];
        let mut sp = 0usize;
        for op in &self.ops {
            match *op {
                Op::Num(v) => {
                    st[sp] = v;
                    sp += 1;
                }
                Op::Var(i) => {
                    st[sp] = vars.get(i as usize).copied().unwrap_or(0.0);
                    sp += 1;
                }
                Op::Neg => st[sp - 1] = -st[sp - 1],
                Op::Bin(b) => {
                    sp -= 1;
                    let (a, c) = (st[sp - 1], st[sp]);
                    st[sp - 1] = binary(b, a, c);
                }
                Op::Call(f, n) => {
                    let n = n as usize;
                    sp -= n;
                    st[sp] = call(f, &st[sp..sp + n], seed);
                    sp += 1;
                }
            }
        }
        st[0]
    }
}

fn truth(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

fn binary(op: BinOp, a: f64, b: f64) -> f64 {
    match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div => a / b,
        // Like a clock: the result has the sign of b (-1 % 3 = 2).
        BinOp::Rem => a - b * (a / b).floor(),
        BinOp::Pow => a.powf(b),
        BinOp::Lt => truth(a < b),
        BinOp::Gt => truth(a > b),
        BinOp::Le => truth(a <= b),
        BinOp::Ge => truth(a >= b),
        BinOp::Eq => truth((a - b).abs() < 1e-9),
        BinOp::Ne => truth((a - b).abs() >= 1e-9),
    }
}

fn call(f: Func, a: &[f64], seed: u32) -> f64 {
    match f {
        Func::Sin => a[0].sin(),
        Func::Cos => a[0].cos(),
        Func::Tan => a[0].tan(),
        Func::Asin => a[0].asin(),
        Func::Acos => a[0].acos(),
        Func::Atan => a[0].atan(),
        Func::Atan2 => a[0].atan2(a[1]),
        Func::Sinh => a[0].sinh(),
        Func::Cosh => a[0].cosh(),
        Func::Tanh => a[0].tanh(),
        Func::Sqrt => a[0].sqrt(),
        Func::Abs => a[0].abs(),
        Func::Exp => a[0].exp(),
        Func::Ln => a[0].ln(),
        Func::Log => a[0].log10(),
        Func::Floor => a[0].floor(),
        Func::Ceil => a[0].ceil(),
        Func::Round => a[0].round(),
        Func::Fract => a[0] - a[0].floor(),
        Func::Sign => {
            if a[0] > 0.0 {
                1.0
            } else if a[0] < 0.0 {
                -1.0
            } else {
                0.0
            }
        }
        Func::Min => a[0].min(a[1]),
        Func::Max => a[0].max(a[1]),
        Func::Pow => a[0].powf(a[1]),
        Func::Mod => binary(BinOp::Rem, a[0], a[1]),
        Func::Hypot => a[0].hypot(a[1]),
        Func::Step => truth(a[1] >= a[0]),
        Func::Clamp => a[0].max(a[1]).min(a[2]),
        Func::Mix => a[0] + (a[1] - a[0]) * a[2],
        Func::Smoothstep => {
            let k = ((a[2] - a[0]) / (a[1] - a[0])).clamp(0.0, 1.0);
            k * k * (3.0 - 2.0 * k)
        }
        Func::If => {
            if a[0] != 0.0 && !a[0].is_nan() {
                a[1]
            } else {
                a[2]
            }
        }
        Func::Rand => rand(a[0], seed),
        Func::Noise => noise(a[0], a[1], seed),
        Func::Fbm => fbm(a[0], a[1], a[2], seed),
    }
}

// ------------------------------------------------------------------------------ seeded randomness

/// A well-mixed 32-bit hash (from the "lowbias32" family).
pub fn hash32(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^= x >> 16;
    x
}

fn unit(h: u32) -> f64 {
    h as f64 / 4_294_967_296.0
}

/// A random number in 0..1 for `x` (the same `x` and seed always give the same number). Numbers
/// that differ only after the third decimal are different inputs.
pub fn rand(x: f64, seed: u32) -> f64 {
    let k = (x * 1000.0).round() as i64;
    unit(hash32((k as u32) ^ hash32((k >> 32) as u32 ^ seed.wrapping_mul(0x9e37_79b9))))
}

fn lattice(ix: i64, iy: i64, seed: u32) -> f64 {
    let h = hash32(ix as u32 ^ hash32(iy as u32 ^ hash32(seed)));
    unit(h) * 2.0 - 1.0
}

/// Smooth value noise in -1..1: random heights on the whole-number grid, eased in between.
pub fn noise(x: f64, y: f64, seed: u32) -> f64 {
    let (fx, fy) = (x.floor(), y.floor());
    let (ix, iy) = (fx as i64, fy as i64);
    let ease = |t: f64| t * t * (3.0 - 2.0 * t);
    let (sx, sy) = (ease(x - fx), ease(y - fy));
    let a = lattice(ix, iy, seed);
    let b = lattice(ix + 1, iy, seed);
    let c = lattice(ix, iy + 1, seed);
    let d = lattice(ix + 1, iy + 1, seed);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sy
}

/// Fractal noise: `octaves` layers of [`noise`], each twice as fine and half as strong. About -1..1.
pub fn fbm(x: f64, y: f64, octaves: f64, seed: u32) -> f64 {
    let n = octaves.round().clamp(1.0, 10.0) as u32;
    let (mut sum, mut amp, mut freq, mut norm) = (0.0, 1.0, 1.0, 0.0);
    for o in 0..n {
        // Each layer is shifted and turned a little so the grids do not line up.
        let (u, v) = (x * freq + o as f64 * 17.3, y * freq - o as f64 * 9.1);
        sum += amp * noise(u * 0.8 - v * 0.6, u * 0.6 + v * 0.8, seed.wrapping_add(o * 101));
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str, x: f64) -> f64 {
        let p = compile(src, &|n| (n == "x").then_some(0)).unwrap();
        p.eval(&[x], 7)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn arithmetic_follows_the_usual_order() {
        assert!(close(run("1 + 2 * 3", 0.0), 7.0));
        assert!(close(run("(1 + 2) * 3", 0.0), 9.0));
        assert!(close(run("2 ^ 3 ^ 2", 0.0), 512.0));
        assert!(close(run("-x^2", 3.0), -9.0));
        assert!(close(run("10 / 4", 0.0), 2.5));
        assert!(close(run("-1 % 3", 0.0), 2.0));
        assert!(close(run("2**3", 0.0), 8.0));
        assert!(close(run("1.5e2 + .5", 0.0), 150.5));
        assert!(close(run("3 - -2", 0.0), 5.0));
    }

    #[test]
    fn side_by_side_multiplies() {
        assert!(close(run("2x", 4.0), 8.0));
        assert!(close(run("3(x+1)", 1.0), 6.0));
        assert!(close(run("2 sin(x)", std::f64::consts::FRAC_PI_2), 2.0));
        assert!(close(run("(x)(x)", 5.0), 25.0));
        assert!(close(run("2pi", 0.0), std::f64::consts::TAU));
        assert!(close(run("2π", 0.0), std::f64::consts::TAU));
        // "2e" is 2 times e, "2e3" is 2000.
        assert!(close(run("2e", 0.0), 2.0 * std::f64::consts::E));
        assert!(close(run("2e3", 0.0), 2000.0));
    }

    #[test]
    fn comparisons_and_if() {
        assert!(close(run("x > 1", 2.0), 1.0));
        assert!(close(run("x <= 1", 2.0), 0.0));
        assert!(close(run("if(x < 0, -1, 1)", -3.0), -1.0));
        assert!(close(run("x == 2", 2.0), 1.0));
        assert!(close(run("x != 2", 2.0), 0.0));
    }

    #[test]
    fn functions_work_and_check_their_arguments() {
        assert!(close(run("max(x, 2) + min(x, 2)", 5.0), 7.0));
        assert!(close(run("clamp(x, 0, 1)", 5.0), 1.0));
        assert!(close(run("mix(0, 10, 0.25)", 0.0), 2.5));
        assert!(close(run("hypot(3, 4)", 0.0), 5.0));
        assert!(close(run("fract(-0.25)", 0.0), 0.75));
        assert!(close(run("atan2(1, 1)", 0.0), std::f64::consts::FRAC_PI_4));
        let slot = |n: &str| (n == "x").then_some(0);
        assert_eq!(compile("sin(1, 2)", &slot).unwrap_err().msg, "Wrong number of values for sin");
        assert_eq!(compile("foo(1)", &slot).unwrap_err().msg, "Unknown function foo");
        assert_eq!(compile("sin", &slot).unwrap_err().msg, "sin needs ( )");
        assert_eq!(compile("a + x", &slot).unwrap_err().msg, "Unknown name a");
    }

    #[test]
    fn errors_say_where() {
        let slot = |_: &str| Some(0);
        let e = compile("1 + ", &slot).unwrap_err();
        assert_eq!(e.msg, "Formula ends too soon");
        assert_eq!(e.at, 4);
        assert_eq!(compile("(1 + 2", &slot).unwrap_err().msg, "Expected )");
        assert_eq!(compile("1 + 2)", &slot).unwrap_err().msg, "Unexpected )");
        assert_eq!(compile("1 $ 2", &slot).unwrap_err(), Error { at: 2, msg: "Unexpected $".into() });
        assert_eq!(compile("  ", &slot).unwrap_err().msg, "Empty formula");
        assert_eq!(compile("1..2", &slot).unwrap_err().msg, "Bad number 1..2");
    }

    #[test]
    fn free_names_are_the_ones_without_a_slot() {
        let known = |n: &str| matches!(n, "x" | "t");
        assert_eq!(free_names("a*sin(b*x + t) + a + pi", known), ["a", "b"]);
        assert_eq!(free_names("x + (", known), Vec::<String>::new(), "a broken formula has none");
        assert_eq!(free_names("θ + k", |n| n == "theta"), ["k"]);
    }

    #[test]
    fn deep_nesting_is_refused_not_crashed() {
        let deep = format!("{}1{}", "(1+".repeat(40), ")".repeat(40));
        assert!(compile(&deep, &|_| None).is_err());
        let fine = format!("{}1{}", "(1+".repeat(10), ")".repeat(10));
        assert!(close(compile(&fine, &|_| None).unwrap().eval(&[], 0), 11.0));
    }

    #[test]
    fn randomness_repeats_for_a_seed_and_changes_with_it() {
        assert_eq!(rand(3.0, 1), rand(3.0, 1));
        assert_ne!(rand(3.0, 1), rand(3.0, 2));
        assert_ne!(rand(3.0, 1), rand(4.0, 1));
        let mut sum = 0.0;
        for i in 0..2000 {
            let r = rand(i as f64, 9);
            assert!((0.0..1.0).contains(&r));
            sum += r;
        }
        assert!((sum / 2000.0 - 0.5).abs() < 0.05, "about even: {}", sum / 2000.0);
        for i in 0..200 {
            let (x, y) = (i as f64 * 0.37, i as f64 * -0.21);
            let n = noise(x, y, 5);
            assert!((-1.0..=1.0).contains(&n));
            assert_eq!(n, noise(x, y, 5));
            assert!((-1.0..=1.0).contains(&fbm(x, y, 5.0, 5)));
        }
        // Noise is smooth: a tiny step makes a tiny change.
        assert!((noise(1.3, 2.7, 4) - noise(1.3001, 2.7, 4)).abs() < 0.01);
        // On the whole-number grid it is the lattice value.
        assert_eq!(noise(2.0, 3.0, 4), lattice(2, 3, 4));
    }
}
