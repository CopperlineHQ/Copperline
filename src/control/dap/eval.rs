// SPDX-License-Identifier: GPL-3.0-or-later

//! The Debug Console / watch expression language, and breakpoint
//! conditions. Small on purpose: registers (`d0`, `a7`, `sp`, `pc`,
//! `sr`), symbols and variables by name, numbers (`$DFF000`, `0x1234`,
//! `1234`, `%1010`, `0b1010`), `+`, `-`, `*` / `/`, the bitwise `&`,
//! `|`, `^`, `<<`, `>>` and `~` on numbers, unary minus, memory reads
//! `[expr]` (long), `[expr].w`, `[expr].b`, and parentheses.
//!
//! The memory-reference forms of the UAE debug adapter (uae-dap, used
//! by the Amiga Assembly VS Code extension for FS-UAE and WinUAE) parse
//! too, so its users' watch expressions carry over: `@(expr[,size])`
//! (unsigned, size 1, 2 or 4, default 4), `@s(expr[,size])` (signed),
//! `#{expr}` (the long at the address) and `${expr}` (the expression
//! itself, a symbol's address).
//!
//! Breakpoint conditions are one comparison between two operands the
//! machine can evaluate itself: `d0 == 5`, `[$100] != 0`, `a0 >= d1`.

use crate::debugger::{BreakCond, CondOp, CondOperand};

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(i64),
    /// A register by DWARF number (0-7 D, 8-15 A, 16 SR, 17 PC).
    Register(u16),
    Name(String),
    Neg(Box<Expr>),
    /// `~expr`: the bitwise complement.
    Not(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Xor(Box<Expr>, Box<Expr>),
    Shl(Box<Expr>, Box<Expr>),
    Shr(Box<Expr>, Box<Expr>),
    /// `[expr].size`: a memory read of 1, 2 or 4 bytes.
    Mem(Box<Expr>, u8),
    /// `value.w` / `value.b`: the value masked to a word or byte.
    Mask(Box<Expr>, i64),
    /// The low `size` bytes of a value, sign-extended (`@s(...)`).
    SignExtend(Box<Expr>, u8),
}

pub fn register_number(name: &str) -> Option<u16> {
    let lower = name.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    match bytes {
        [b'd', n @ b'0'..=b'7'] => Some(u16::from(n - b'0')),
        [b'a', n @ b'0'..=b'7'] => Some(8 + u16::from(n - b'0')),
        b"sp" => Some(15),
        b"fp" => Some(14),
        b"sr" => Some(16),
        b"pc" => Some(17),
        _ => None,
    }
}

pub fn register_name(reg: u16) -> String {
    match reg {
        0..=7 => format!("d{reg}"),
        8..=15 => format!("a{}", reg - 8),
        16 => "sr".into(),
        17 => "pc".into(),
        _ => format!("r{reg}"),
    }
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// Nesting of the recursive descent, bounded so a client cannot
    /// overflow the stack with parentheses or unary prefixes.
    depth: u32,
}

const MAX_DEPTH: u32 = 64;

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn eat(&mut self, want: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(want) {
            self.pos += want.len_utf8();
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("expression nested too deeply".into());
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Expr, String> {
        self.enter()?;
        let result = self.expr_inner();
        self.depth -= 1;
        result
    }

    /// `|`, `^`, `&`, then the shifts, binding more tightly in that
    /// order, as in C.
    fn expr_inner(&mut self) -> Result<Expr, String> {
        let mut lhs = self.bit_xor()?;
        while self.eat_op("|", "|") {
            let rhs = self.bit_xor()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn bit_xor(&mut self) -> Result<Expr, String> {
        let mut lhs = self.bit_and()?;
        while self.eat_op("^", "") {
            let rhs = self.bit_and()?;
            lhs = Expr::Xor(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn bit_and(&mut self) -> Result<Expr, String> {
        let mut lhs = self.shift()?;
        while self.eat_op("&", "&") {
            let rhs = self.shift()?;
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn shift(&mut self) -> Result<Expr, String> {
        let mut lhs = self.additive()?;
        loop {
            if self.eat_op("<<", "") {
                let rhs = self.additive()?;
                lhs = Expr::Shl(Box::new(lhs), Box::new(rhs));
            } else if self.eat_op(">>", "") {
                let rhs = self.additive()?;
                lhs = Expr::Shr(Box::new(lhs), Box::new(rhs));
            } else {
                return Ok(lhs);
            }
        }
    }

    /// Consume the operator `op` unless it is followed by `unless` (so
    /// `&&` and `||` are not taken for `&` and `|`).
    fn eat_op(&mut self, op: &str, unless: &str) -> bool {
        self.skip_ws();
        let rest = &self.src[self.pos..];
        if !rest.starts_with(op) || (!unless.is_empty() && rest[op.len()..].starts_with(unless)) {
            return false;
        }
        self.pos += op.len();
        true
    }

    fn additive(&mut self) -> Result<Expr, String> {
        let mut lhs = self.term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.pos += 1;
                    let rhs = self.term()?;
                    lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
                }
                Some('-') => {
                    self.pos += 1;
                    let rhs = self.term()?;
                    lhs = Expr::Sub(Box::new(lhs), Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn term(&mut self) -> Result<Expr, String> {
        let mut lhs = self.unary()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
                }
                Some('/') => {
                    self.pos += 1;
                    let rhs = self.unary()?;
                    lhs = Expr::Div(Box::new(lhs), Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn unary(&mut self) -> Result<Expr, String> {
        self.enter()?;
        let result = self.unary_inner();
        self.depth -= 1;
        result
    }

    fn unary_inner(&mut self) -> Result<Expr, String> {
        self.skip_ws();
        match self.peek() {
            Some('-') => {
                self.pos += 1;
                Ok(Expr::Neg(Box::new(self.unary()?)))
            }
            Some('~') => {
                self.pos += 1;
                Ok(Expr::Not(Box::new(self.unary()?)))
            }
            Some('*') => {
                // C-style deref: a long.
                self.pos += 1;
                Ok(Expr::Mem(Box::new(self.unary()?), 4))
            }
            _ => self.postfix(),
        }
    }

    fn postfix(&mut self) -> Result<Expr, String> {
        let mut e = self.primary()?;
        loop {
            self.skip_ws();
            if self.src[self.pos..].starts_with(".w") || self.src[self.pos..].starts_with(".W") {
                self.pos += 2;
                e = resize(e, 2)?;
            } else if self.src[self.pos..].starts_with(".b")
                || self.src[self.pos..].starts_with(".B")
            {
                self.pos += 2;
                e = resize(e, 1)?;
            } else if self.src[self.pos..].starts_with(".l")
                || self.src[self.pos..].starts_with(".L")
            {
                self.pos += 2;
                e = resize(e, 4)?;
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, String> {
        self.skip_ws();
        let Some(c) = self.peek() else {
            return Err("unexpected end of expression".into());
        };
        if c == '(' {
            self.pos += 1;
            let e = self.expr()?;
            if !self.eat(')') {
                return Err("missing ')'".into());
            }
            return Ok(e);
        }
        if c == '[' {
            self.pos += 1;
            let e = self.expr()?;
            if !self.eat(']') {
                return Err("missing ']'".into());
            }
            return Ok(Expr::Mem(Box::new(e), 4));
        }
        if c == '$' {
            self.pos += 1;
            // uae-dap's `${expr}`: the expression itself.
            if self.peek() == Some('{') {
                return self.braced();
            }
            return self.number(16);
        }
        if c == '#' {
            // uae-dap's `#{expr}`: the long at the address.
            self.pos += 1;
            if self.peek() != Some('{') {
                return Err("expected '{' after '#'".into());
            }
            return Ok(Expr::Mem(Box::new(self.braced()?), 4));
        }
        if c == '@' {
            self.pos += 1;
            return self.memory_reference();
        }
        if c == '%' {
            self.pos += 1;
            return self.number(2);
        }
        if c.is_ascii_digit() {
            let rest = &self.src[self.pos..];
            if rest.starts_with("0x") || rest.starts_with("0X") {
                self.pos += 2;
                return self.number(16);
            }
            if rest.starts_with("0b") || rest.starts_with("0B") {
                self.pos += 2;
                return self.number(2);
            }
            return self.number(10);
        }
        if c.is_alphabetic() || c == '_' {
            let start = self.pos;
            while let Some(c) = self.peek() {
                if c.is_alphanumeric() || c == '_' {
                    self.pos += c.len_utf8();
                } else {
                    break;
                }
            }
            let name = &self.src[start..self.pos];
            return Ok(match register_number(name) {
                Some(reg) => Expr::Register(reg),
                None => Expr::Name(name.to_string()),
            });
        }
        Err(format!("unexpected '{c}'"))
    }

    /// `{expr}`, with the parser at the opening brace.
    fn braced(&mut self) -> Result<Expr, String> {
        self.pos += 1;
        let e = self.expr()?;
        if !self.eat('}') {
            return Err("missing '}'".into());
        }
        Ok(e)
    }

    /// `(expr[,size])`, `s(expr[,size])` or `u(expr[,size])` after `@`:
    /// a memory read of 1, 2 or 4 bytes (default 4), sign-extended for
    /// `s`.
    fn memory_reference(&mut self) -> Result<Expr, String> {
        let signed = match self.peek() {
            Some('s' | 'S') => {
                self.pos += 1;
                true
            }
            Some('u' | 'U') => {
                self.pos += 1;
                false
            }
            _ => false,
        };
        if self.peek() != Some('(') {
            return Err("expected '(' after '@'".into());
        }
        self.pos += 1;
        let address = self.expr()?;
        let size = if self.eat(',') {
            self.skip_ws();
            match self.number(10)? {
                Expr::Number(n @ (1 | 2 | 4)) => n as u8,
                _ => return Err("memory reference size must be 1, 2 or 4".into()),
            }
        } else {
            4
        };
        if !self.eat(')') {
            return Err("missing ')'".into());
        }
        let read = Expr::Mem(Box::new(address), size);
        Ok(if signed {
            Expr::SignExtend(Box::new(read), size)
        } else {
            read
        })
    }

    fn number(&mut self, radix: u32) -> Result<Expr, String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_digit(radix) || c == '_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text: String = self.src[start..self.pos]
            .chars()
            .filter(|c| *c != '_')
            .collect();
        if text.is_empty() {
            return Err("expected digits".into());
        }
        i64::from_str_radix(&text, radix)
            .map(Expr::Number)
            .map_err(|e| format!("bad number {text}: {e}"))
    }
}

/// A size suffix: re-sizes a memory read, masks any other value.
fn resize(e: Expr, size: u8) -> Result<Expr, String> {
    match e {
        Expr::Mem(inner, _) => Ok(Expr::Mem(inner, size)),
        other => Ok(Expr::Mask(
            Box::new(other),
            match size {
                1 => 0xFF,
                2 => 0xFFFF,
                _ => 0xFFFF_FFFF,
            },
        )),
    }
}

pub fn parse(text: &str) -> Result<Expr, String> {
    let mut p = Parser {
        src: text.trim(),
        pos: 0,
        depth: 0,
    };
    let e = p.expr()?;
    p.skip_ws();
    if p.pos != p.src.len() {
        return Err(format!("unexpected '{}'", &p.src[p.pos..]));
    }
    Ok(e)
}

/// What an evaluation needs from the machine and the program.
pub trait Env {
    fn register(&mut self, reg: u16) -> Result<i64, String>;
    fn name(&mut self, name: &str) -> Result<i64, String>;
    fn read(&mut self, addr: u32, size: u8) -> Result<i64, String>;
}

pub fn eval(e: &Expr, env: &mut dyn Env) -> Result<i64, String> {
    Ok(match e {
        Expr::Number(n) => *n,
        Expr::Register(r) => env.register(*r)?,
        Expr::Name(n) => env.name(n)?,
        Expr::Neg(inner) => eval(inner, env)?.wrapping_neg(),
        Expr::Not(inner) => !eval(inner, env)?,
        Expr::Add(a, b) => eval(a, env)?.wrapping_add(eval(b, env)?),
        Expr::Sub(a, b) => eval(a, env)?.wrapping_sub(eval(b, env)?),
        Expr::Mul(a, b) => eval(a, env)?.wrapping_mul(eval(b, env)?),
        Expr::And(a, b) => eval(a, env)? & eval(b, env)?,
        Expr::Or(a, b) => eval(a, env)? | eval(b, env)?,
        Expr::Xor(a, b) => eval(a, env)? ^ eval(b, env)?,
        // A count outside 0..64 shifts every bit out (leaving 0, or -1
        // for a negative value shifted right) rather than wrapping.
        Expr::Shl(a, b) => {
            let (value, count) = (eval(a, env)?, eval(b, env)?);
            if (0..64).contains(&count) {
                value << count
            } else {
                0
            }
        }
        Expr::Shr(a, b) => {
            let (value, count) = (eval(a, env)?, eval(b, env)?);
            if (0..64).contains(&count) {
                value >> count
            } else if value < 0 {
                -1
            } else {
                0
            }
        }
        Expr::SignExtend(inner, size) => {
            let v = eval(inner, env)?;
            match size {
                1 => i64::from(v as u8 as i8),
                2 => i64::from(v as u16 as i16),
                _ => i64::from(v as u32 as i32),
            }
        }
        Expr::Div(a, b) => {
            let d = eval(b, env)?;
            if d == 0 {
                return Err("division by zero".into());
            }
            eval(a, env)?.wrapping_div(d)
        }
        Expr::Mask(inner, mask) => eval(inner, env)? & mask,
        Expr::Mem(inner, size) => {
            let addr = eval(inner, env)?;
            let addr = u32::try_from(addr & 0xFFFF_FFFF).map_err(|_| "bad address".to_string())?;
            env.read(addr, *size)?
        }
    })
}

/// A breakpoint condition: `lhs OP rhs` with the machine's operand set.
pub fn parse_condition(text: &str) -> Result<BreakCond, String> {
    let ops: [(&str, CondOp); 8] = [
        ("==", CondOp::Eq),
        ("!=", CondOp::Ne),
        ("<=", CondOp::Le),
        (">=", CondOp::Ge),
        ("<", CondOp::Lt),
        (">", CondOp::Gt),
        ("&", CondOp::And),
        ("=", CondOp::Eq),
    ];
    for (token, op) in ops {
        if let Some((lhs, rhs)) = text.split_once(token) {
            let lhs = cond_operand(lhs.trim())?;
            let rhs = cond_operand(rhs.trim())?;
            return Ok(BreakCond { lhs, op, rhs });
        }
    }
    Err(format!("no comparison in {text:?}"))
}

fn cond_operand(text: &str) -> Result<CondOperand, String> {
    if let Some(reg) = register_number(text) {
        return Ok(match reg {
            0..=7 => CondOperand::Data(usize::from(reg)),
            8..=15 => CondOperand::Addr(usize::from(reg - 8)),
            16 => CondOperand::Sr,
            _ => CondOperand::Pc,
        });
    }
    let imm = |n: i64| {
        u32::try_from(n)
            .or_else(|_| i32::try_from(n).map(|v| v as u32))
            .map(CondOperand::Imm)
            .map_err(|_| format!("{n} does not fit a 32-bit operand"))
    };
    match parse(text)? {
        Expr::Number(n) => imm(n),
        Expr::Neg(inner) => match *inner {
            Expr::Number(n) => imm(n.wrapping_neg()),
            _ => Err(format!(
                "operand {text:?} must be a register, number or [address]"
            )),
        },
        // The machine compares the 16-bit word at the address: `[addr]`
        // and `[addr].w`; other sizes would be silently wrong.
        Expr::Mem(inner, size) => match (*inner, size) {
            (Expr::Number(addr), 2 | 4) => u32::try_from(addr)
                .map(CondOperand::Mem)
                .map_err(|_| format!("{addr} is not an address")),
            (Expr::Number(_), _) => {
                Err("memory operands compare a 16-bit word ([addr] or [addr].w)".into())
            }
            _ => Err("memory operands need a constant address".into()),
        },
        _ => Err(format!(
            "operand {text:?} must be a register, number or [address]"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;

    impl Env for Fake {
        fn register(&mut self, reg: u16) -> Result<i64, String> {
            Ok(match reg {
                0 => 0x1234_5678,
                15 => 0x8000,
                17 => 0x1000,
                r => i64::from(r),
            })
        }
        fn name(&mut self, name: &str) -> Result<i64, String> {
            match name {
                "counter" => Ok(5),
                "_start" => Ok(0x2000),
                _ => Err(format!("unknown {name}")),
            }
        }
        fn read(&mut self, addr: u32, size: u8) -> Result<i64, String> {
            Ok(i64::from(addr) + i64::from(size))
        }
    }

    fn run(text: &str) -> Result<i64, String> {
        eval(&parse(text)?, &mut Fake)
    }

    #[test]
    fn arithmetic_registers_and_names() {
        assert_eq!(run("$10 + 0x10 + 16"), Ok(48));
        assert_eq!(run("d0"), Ok(0x1234_5678));
        assert_eq!(run("sp - 4"), Ok(0x7FFC));
        assert_eq!(run("counter * 2 + _start"), Ok(0x200A));
        assert_eq!(run("-(3)"), Ok(-3));
        assert_eq!(run("%101"), Ok(5));
    }

    #[test]
    fn memory_reads_carry_their_size() {
        assert_eq!(run("[$100]"), Ok(0x104));
        assert_eq!(run("[$100].w"), Ok(0x102));
        assert_eq!(run("[$100].b"), Ok(0x101));
        assert_eq!(run("*sp"), Ok(0x8004));
        assert_eq!(run("d0.w"), Ok(0x5678));
        assert_eq!(run("d0.b"), Ok(0x78));
    }

    #[test]
    fn bitwise_operators_bind_like_c() {
        assert_eq!(run("$F0 & $3C"), Ok(0x30));
        assert_eq!(run("$F0 | $0F"), Ok(0xFF));
        assert_eq!(run("$FF ^ $0F"), Ok(0xF0));
        assert_eq!(run("1 << 4 + 1"), Ok(32));
        assert_eq!(run("$100 >> 4"), Ok(0x10));
        assert_eq!(run("1 | 2 & 3"), Ok(3));
        assert_eq!(run("~0 & $FFFF"), Ok(0xFFFF));
        assert_eq!(run("1 << 64"), Ok(0));
        assert_eq!(run("-1 >> 70"), Ok(-1));
        assert_eq!(run("0b1010"), Ok(10));
        assert!(run("1 && 2").is_err());
        assert!(run("1 || 2").is_err());
    }

    #[test]
    fn uae_dap_memory_references_parse() {
        // Fake reads return address + size.
        assert_eq!(run("@($100)"), Ok(0x104));
        assert_eq!(run("@($100,2)"), Ok(0x102));
        assert_eq!(run("@u($100, 1)"), Ok(0x101));
        // $FE + 1 = $FF read as a signed byte.
        assert_eq!(run("@s($FE,1)"), Ok(-1));
        assert_eq!(run("#{$100}"), Ok(0x104));
        assert_eq!(run("${_start} + 2"), Ok(0x2002));
        assert_eq!(run("@(d0.w + 2, 4)"), Ok(0x567E));
        assert!(run("@($100,3)").is_err());
        assert!(run("@[$100]").is_err());
        assert!(run("#$100").is_err());
        assert!(run("${_start").is_err());
    }

    #[test]
    fn errors_are_reported() {
        assert!(run("1 +").is_err());
        assert!(run("nothing").is_err());
        assert!(run("1 / 0").is_err());
        assert!(run("(1").is_err());
    }

    #[test]
    fn conditions_use_the_machine_operand_set() {
        let c = parse_condition("d0 == 5").unwrap();
        assert_eq!(c.lhs, CondOperand::Data(0));
        assert_eq!(c.op, CondOp::Eq);
        assert_eq!(c.rhs, CondOperand::Imm(5));
        let c = parse_condition("[$DFF006] != a1").unwrap();
        assert_eq!(c.lhs, CondOperand::Mem(0xDFF006));
        assert_eq!(c.rhs, CondOperand::Addr(1));
        let c = parse_condition("sr & $2000").unwrap();
        assert_eq!(c.op, CondOp::And);
        assert_eq!(c.lhs, CondOperand::Sr);
        assert_eq!(parse_condition("pc == 4").unwrap().lhs, CondOperand::Pc);
        assert!(parse_condition("d0 + 1 == 2").is_err());
    }
}
