//! Math utility: bc.

use crate::helpers::get_input_text;
use crate::UtilContext;

// ---------------------------------------------------------------------------
// bc — basic calculator
// ---------------------------------------------------------------------------

pub(crate) fn util_bc(ctx: &mut UtilContext<'_>, argv: &[&str]) -> i32 {
    let mut args = &argv[1..];
    let mut load_math_lib = false;

    while let Some(arg) = args.first() {
        match *arg {
            "-l" => {
                load_math_lib = true;
                args = &args[1..];
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                let msg = format!("bc: unknown option '{arg}'\n");
                ctx.output.stderr(msg.as_bytes());
                return 1;
            }
            _ => break,
        }
    }

    let input = get_input_text(ctx, args);

    let mut env = BcEnv::new();
    if load_math_lib {
        env.scale = 20;
        // Pre-define math library functions (handled in function evaluation)
        env.math_lib = true;
    }

    let stmts = bc_parse(&input);
    for stmt in &stmts {
        if let Err(e) = bc_run(ctx, &mut env, stmt) {
            let msg = format!("bc: {e}\n");
            ctx.output.stderr(msg.as_bytes());
            return 1;
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Environment
// ---------------------------------------------------------------------------

struct BcEnv {
    vars: Vec<(String, f64)>,
    scale: i32,
    ibase: i32,
    obase: i32,
    math_lib: bool,
    /// User-defined functions: (name, params, body)
    functions: Vec<(String, Vec<String>, Vec<BcStmt>)>,
}

impl BcEnv {
    fn new() -> Self {
        Self {
            vars: Vec::new(),
            scale: 0,
            ibase: 10,
            obase: 10,
            math_lib: false,
            functions: Vec::new(),
        }
    }

    fn get_var(&self, name: &str) -> f64 {
        for (n, v) in self.vars.iter().rev() {
            if n == name {
                return *v;
            }
        }
        0.0
    }

    fn set_var(&mut self, name: &str, val: f64) {
        for (n, v) in self.vars.iter_mut().rev() {
            if n == name {
                *v = val;
                return;
            }
        }
        self.vars.push((name.to_string(), val));
    }
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum BcExpr {
    Num(f64),
    Var(String),
    BinOp(Box<BcExpr>, BcOp, Box<BcExpr>),
    UnaryMinus(Box<BcExpr>),
    Assign(String, Box<BcExpr>),
    FnCall(String, Vec<BcExpr>),
    CompoundAssign(String, BcOp, Box<BcExpr>),
    PreIncr(String),
    PreDecr(String),
    PostIncr(String),
    PostDecr(String),
}

#[derive(Debug, Clone, Copy)]
enum BcOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

#[derive(Debug, Clone)]
enum BcStmt {
    Expr(BcExpr),
    If(BcExpr, Vec<BcStmt>),
    While(BcExpr, Vec<BcStmt>),
    For(Box<BcStmt>, BcExpr, Box<BcStmt>, Vec<BcStmt>),
    Print(BcExpr),
    Quit,
    FnDef(String, Vec<String>, Vec<BcStmt>),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct BcParser<'a> {
    input: &'a str,
    pos: usize,
}

impl<'a> BcParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, pos: 0 }
    }

    fn remaining(&self) -> &'a str {
        &self.input[self.pos..]
    }

    fn skip_ws(&mut self) {
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b == b' ' || b == b'\t' || b == b'\r' {
                self.pos += 1;
            } else if self.remaining().starts_with("/*") {
                // Block comment
                if let Some(end) = self.remaining()[2..].find("*/") {
                    self.pos += end + 4;
                } else {
                    self.pos = self.input.len();
                }
            } else if b == b'#' {
                // Line comment
                while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn advance(&mut self) {
        if self.pos < self.input.len() {
            self.pos += 1;
        }
    }

    fn eat(&mut self, ch: u8) -> bool {
        self.skip_ws();
        if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == ch {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_str(&mut self, s: &str) -> bool {
        self.skip_ws();
        if self.remaining().starts_with(s) {
            // For keywords, ensure the next char is not alphanumeric (to avoid matching prefixes)
            let after = self.pos + s.len();
            if s.chars().all(|c| c.is_ascii_alphabetic()) && after < self.input.len() {
                let next = self.input.as_bytes()[after];
                if next.is_ascii_alphanumeric() || next == b'_' {
                    return false;
                }
            }
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn parse_ident(&mut self) -> Option<String> {
        self.skip_ws();
        let start = self.pos;
        while self.pos < self.input.len()
            && (self.input.as_bytes()[self.pos].is_ascii_alphanumeric()
                || self.input.as_bytes()[self.pos] == b'_')
        {
            self.pos += 1;
        }
        if self.pos > start {
            Some(self.input[start..self.pos].to_string())
        } else {
            None
        }
    }

    fn parse_number(&mut self, ibase: i32) -> Option<f64> {
        self.skip_ws();
        let start = self.pos;
        let mut has_dot = false;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b.is_ascii_digit()
                || (b == b'.' && !has_dot)
                || (ibase > 10 && b.is_ascii_hexdigit())
            {
                if b == b'.' {
                    has_dot = true;
                }
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos > start {
            let s = &self.input[start..self.pos];
            if ibase != 10 && !has_dot {
                let val = i64_from_base(s, ibase);
                Some(val as f64)
            } else {
                s.parse::<f64>().ok()
            }
        } else {
            None
        }
    }

    fn parse_expr(&mut self, ibase: i32) -> Option<BcExpr> {
        self.parse_assignment(ibase)
    }

    fn parse_assignment(&mut self, ibase: i32) -> Option<BcExpr> {
        let expr = self.parse_comparison(ibase)?;

        // Check for assignment operators
        if let BcExpr::Var(ref name) = expr {
            let name = name.clone();
            self.skip_ws();
            if self.eat_str("+=") {
                let rhs = self.parse_assignment(ibase)?;
                return Some(BcExpr::CompoundAssign(name, BcOp::Add, Box::new(rhs)));
            }
            if self.eat_str("-=") {
                let rhs = self.parse_assignment(ibase)?;
                return Some(BcExpr::CompoundAssign(name, BcOp::Sub, Box::new(rhs)));
            }
            if self.eat_str("*=") {
                let rhs = self.parse_assignment(ibase)?;
                return Some(BcExpr::CompoundAssign(name, BcOp::Mul, Box::new(rhs)));
            }
            if self.eat_str("/=") {
                let rhs = self.parse_assignment(ibase)?;
                return Some(BcExpr::CompoundAssign(name, BcOp::Div, Box::new(rhs)));
            }
            if self.eat_str("%=") {
                let rhs = self.parse_assignment(ibase)?;
                return Some(BcExpr::CompoundAssign(name, BcOp::Mod, Box::new(rhs)));
            }
            // Simple assignment (but not ==)
            self.skip_ws();
            if self.pos < self.input.len()
                && self.input.as_bytes()[self.pos] == b'='
                && self.input.as_bytes().get(self.pos + 1) != Some(&b'=')
            {
                self.pos += 1;
                let rhs = self.parse_assignment(ibase)?;
                return Some(BcExpr::Assign(name, Box::new(rhs)));
            }
        }
        Some(expr)
    }

    fn parse_comparison(&mut self, ibase: i32) -> Option<BcExpr> {
        let mut left = self.parse_add_sub(ibase)?;
        loop {
            self.skip_ws();
            let op = if self.eat_str("==") {
                BcOp::Eq
            } else if self.eat_str("!=") {
                BcOp::Ne
            } else if self.eat_str("<=") {
                BcOp::Le
            } else if self.eat_str(">=") {
                BcOp::Ge
            } else if self.eat(b'<') {
                BcOp::Lt
            } else if self.eat(b'>') {
                BcOp::Gt
            } else {
                break;
            };
            let right = self.parse_add_sub(ibase)?;
            left = BcExpr::BinOp(Box::new(left), op, Box::new(right));
        }
        Some(left)
    }

    /// Check if the next non-whitespace char is `ch` but NOT followed by `=` (to avoid
    /// consuming `+=`, `-=`, `*=`, `/=`, `%=` as binary ops).
    fn eat_op(&mut self, ch: u8) -> bool {
        self.skip_ws();
        if self.pos < self.input.len() && self.input.as_bytes()[self.pos] == ch {
            if self.input.as_bytes().get(self.pos + 1) == Some(&b'=') {
                return false; // This is a compound assignment, don't consume
            }
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_add_sub(&mut self, ibase: i32) -> Option<BcExpr> {
        let mut left = self.parse_mul_div(ibase)?;
        loop {
            self.skip_ws();
            if self.eat_op(b'+') {
                let right = self.parse_mul_div(ibase)?;
                left = BcExpr::BinOp(Box::new(left), BcOp::Add, Box::new(right));
            } else if self.eat_op(b'-') {
                let right = self.parse_mul_div(ibase)?;
                left = BcExpr::BinOp(Box::new(left), BcOp::Sub, Box::new(right));
            } else {
                break;
            }
        }
        Some(left)
    }

    fn parse_mul_div(&mut self, ibase: i32) -> Option<BcExpr> {
        let mut left = self.parse_power(ibase)?;
        loop {
            self.skip_ws();
            if self.eat_op(b'*') {
                let right = self.parse_power(ibase)?;
                left = BcExpr::BinOp(Box::new(left), BcOp::Mul, Box::new(right));
            } else if self.eat_op(b'/') {
                let right = self.parse_power(ibase)?;
                left = BcExpr::BinOp(Box::new(left), BcOp::Div, Box::new(right));
            } else if self.eat_op(b'%') {
                let right = self.parse_power(ibase)?;
                left = BcExpr::BinOp(Box::new(left), BcOp::Mod, Box::new(right));
            } else {
                break;
            }
        }
        Some(left)
    }

    fn parse_power(&mut self, ibase: i32) -> Option<BcExpr> {
        let base_expr = self.parse_unary(ibase)?;
        self.skip_ws();
        if self.eat(b'^') {
            let exp = self.parse_power(ibase)?; // right-associative
            Some(BcExpr::BinOp(Box::new(base_expr), BcOp::Pow, Box::new(exp)))
        } else {
            Some(base_expr)
        }
    }

    fn parse_unary(&mut self, ibase: i32) -> Option<BcExpr> {
        self.skip_ws();
        // Pre-increment/decrement
        if self.eat_str("++") {
            let name = self.parse_ident()?;
            return Some(BcExpr::PreIncr(name));
        }
        if self.eat_str("--") {
            let name = self.parse_ident()?;
            return Some(BcExpr::PreDecr(name));
        }
        if self.eat(b'-') {
            let expr = self.parse_primary(ibase)?;
            return Some(BcExpr::UnaryMinus(Box::new(expr)));
        }
        let expr = self.parse_primary(ibase)?;
        // Post-increment/decrement
        if let BcExpr::Var(ref name) = expr {
            let name = name.clone();
            if self.eat_str("++") {
                return Some(BcExpr::PostIncr(name));
            }
            if self.eat_str("--") {
                return Some(BcExpr::PostDecr(name));
            }
        }
        Some(expr)
    }

    fn parse_primary(&mut self, ibase: i32) -> Option<BcExpr> {
        self.skip_ws();
        if self.pos >= self.input.len() {
            return None;
        }

        let b = self.input.as_bytes()[self.pos];

        // Parenthesized expression
        if b == b'(' {
            self.advance();
            let expr = self.parse_expr(ibase)?;
            self.eat(b')');
            return Some(expr);
        }

        // Number
        if b.is_ascii_digit() || b == b'.' {
            let n = self.parse_number(ibase)?;
            return Some(BcExpr::Num(n));
        }

        // Identifier or function call
        if b.is_ascii_alphabetic() || b == b'_' {
            let name = self.parse_ident()?;
            self.skip_ws();
            if self.eat(b'(') {
                // Function call
                let mut func_args = Vec::new();
                if !self.eat(b')') {
                    if let Some(arg) = self.parse_expr(ibase) {
                        func_args.push(arg);
                    }
                    while self.eat(b',') {
                        if let Some(arg) = self.parse_expr(ibase) {
                            func_args.push(arg);
                        }
                    }
                    self.eat(b')');
                }
                return Some(BcExpr::FnCall(name, func_args));
            }
            return Some(BcExpr::Var(name));
        }

        None
    }

    fn parse_block(&mut self, ibase: i32) -> Vec<BcStmt> {
        let mut stmts = Vec::new();
        self.skip_ws();
        if self.eat(b'{') {
            while !self.eat(b'}') {
                if self.pos >= self.input.len() {
                    break;
                }
                if let Some(s) = self.parse_stmt(ibase) {
                    stmts.push(s);
                } else {
                    self.skip_separator();
                }
            }
        } else if let Some(s) = self.parse_stmt(ibase) {
            stmts.push(s);
        }
        stmts
    }

    fn skip_separator(&mut self) {
        self.skip_ws();
        if self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if b == b';' || b == b'\n' {
                self.pos += 1;
            }
        }
    }

    fn parse_stmt(&mut self, ibase: i32) -> Option<BcStmt> {
        self.skip_ws();
        // Skip empty separators
        while self.pos < self.input.len()
            && (self.input.as_bytes()[self.pos] == b';' || self.input.as_bytes()[self.pos] == b'\n')
        {
            self.pos += 1;
            self.skip_ws();
        }

        if self.pos >= self.input.len() {
            return None;
        }

        // Check for keywords
        if self.eat_str("quit") {
            self.skip_separator();
            return Some(BcStmt::Quit);
        }

        if self.eat_str("if") {
            self.skip_ws();
            self.eat(b'(');
            let cond = self.parse_expr(ibase)?;
            self.eat(b')');
            let body = self.parse_block(ibase);
            return Some(BcStmt::If(cond, body));
        }

        if self.eat_str("while") {
            self.skip_ws();
            self.eat(b'(');
            let cond = self.parse_expr(ibase)?;
            self.eat(b')');
            let body = self.parse_block(ibase);
            return Some(BcStmt::While(cond, body));
        }

        if self.eat_str("for") {
            self.skip_ws();
            self.eat(b'(');
            let init = self
                .parse_stmt(ibase)
                .unwrap_or(BcStmt::Expr(BcExpr::Num(0.0)));
            self.eat(b';');
            let cond = self.parse_expr(ibase).unwrap_or(BcExpr::Num(1.0));
            self.eat(b';');
            let incr = self
                .parse_stmt(ibase)
                .unwrap_or(BcStmt::Expr(BcExpr::Num(0.0)));
            self.eat(b')');
            let body = self.parse_block(ibase);
            return Some(BcStmt::For(Box::new(init), cond, Box::new(incr), body));
        }

        if self.eat_str("print") {
            self.skip_ws();
            let expr = self.parse_expr(ibase)?;
            self.skip_separator();
            return Some(BcStmt::Print(expr));
        }

        if self.eat_str("define") {
            self.skip_ws();
            let name = self.parse_ident()?;
            self.eat(b'(');
            let mut params = Vec::new();
            if !self.eat(b')') {
                if let Some(p) = self.parse_ident() {
                    params.push(p);
                }
                while self.eat(b',') {
                    if let Some(p) = self.parse_ident() {
                        params.push(p);
                    }
                }
                self.eat(b')');
            }
            let body = self.parse_block(ibase);
            return Some(BcStmt::FnDef(name, params, body));
        }

        // Expression statement
        let expr = self.parse_expr(ibase)?;
        self.skip_separator();
        Some(BcStmt::Expr(expr))
    }
}

fn bc_parse(input: &str) -> Vec<BcStmt> {
    let mut parser = BcParser::new(input);
    let mut stmts = Vec::new();
    while parser.pos < parser.input.len() {
        if let Some(s) = parser.parse_stmt(10) {
            stmts.push(s);
        } else {
            parser.skip_separator();
            if parser.pos < parser.input.len() {
                parser.advance(); // skip unrecognized char
            }
        }
    }
    stmts
}

// ---------------------------------------------------------------------------
// Expression evaluator
// ---------------------------------------------------------------------------

fn compute(ctx: &mut UtilContext<'_>, env: &mut BcEnv, expr: &BcExpr) -> Result<f64, String> {
    match expr {
        BcExpr::Num(n) => Ok(*n),
        BcExpr::Var(name) => match name.as_str() {
            "scale" => Ok(f64::from(env.scale)),
            "ibase" => Ok(f64::from(env.ibase)),
            "obase" => Ok(f64::from(env.obase)),
            _ => Ok(env.get_var(name)),
        },
        BcExpr::UnaryMinus(e) => {
            let v = compute(ctx, env, e)?;
            Ok(-v)
        }
        BcExpr::BinOp(left, op, right) => {
            let l = compute(ctx, env, left)?;
            let r = compute(ctx, env, right)?;
            match op {
                BcOp::Add => Ok(l + r),
                BcOp::Sub => Ok(l - r),
                BcOp::Mul => Ok(l * r),
                BcOp::Div => {
                    if r == 0.0 {
                        Err("division by zero".to_string())
                    } else {
                        Ok(l / r)
                    }
                }
                BcOp::Mod => {
                    if r == 0.0 {
                        Err("modulo by zero".to_string())
                    } else {
                        Ok(l % r)
                    }
                }
                BcOp::Pow => Ok(l.powf(r)),
                BcOp::Eq => Ok(if (l - r).abs() < f64::EPSILON {
                    1.0
                } else {
                    0.0
                }),
                BcOp::Ne => Ok(if (l - r).abs() >= f64::EPSILON {
                    1.0
                } else {
                    0.0
                }),
                BcOp::Lt => Ok(if l < r { 1.0 } else { 0.0 }),
                BcOp::Gt => Ok(if l > r { 1.0 } else { 0.0 }),
                BcOp::Le => Ok(if l <= r { 1.0 } else { 0.0 }),
                BcOp::Ge => Ok(if l >= r { 1.0 } else { 0.0 }),
            }
        }
        BcExpr::Assign(name, rhs) => {
            let val = compute(ctx, env, rhs)?;
            match name.as_str() {
                "scale" => env.scale = val as i32,
                "ibase" => {
                    let b = val as i32;
                    if (2..=16).contains(&b) {
                        env.ibase = b;
                    }
                }
                "obase" => {
                    let b = val as i32;
                    if (2..=16).contains(&b) {
                        env.obase = b;
                    }
                }
                _ => env.set_var(name, val),
            }
            Ok(val)
        }
        BcExpr::CompoundAssign(name, op, rhs) => {
            let current = env.get_var(name);
            let rval = compute(ctx, env, rhs)?;
            let result = match op {
                BcOp::Add => current + rval,
                BcOp::Sub => current - rval,
                BcOp::Mul => current * rval,
                BcOp::Div => {
                    if rval == 0.0 {
                        return Err("division by zero".to_string());
                    }
                    current / rval
                }
                BcOp::Mod => {
                    if rval == 0.0 {
                        return Err("modulo by zero".to_string());
                    }
                    current % rval
                }
                _ => current,
            };
            env.set_var(name, result);
            Ok(result)
        }
        BcExpr::PreIncr(name) => {
            let v = env.get_var(name) + 1.0;
            env.set_var(name, v);
            Ok(v)
        }
        BcExpr::PreDecr(name) => {
            let v = env.get_var(name) - 1.0;
            env.set_var(name, v);
            Ok(v)
        }
        BcExpr::PostIncr(name) => {
            let v = env.get_var(name);
            env.set_var(name, v + 1.0);
            Ok(v)
        }
        BcExpr::PostDecr(name) => {
            let v = env.get_var(name);
            env.set_var(name, v - 1.0);
            Ok(v)
        }
        BcExpr::FnCall(name, func_args) => call_function(ctx, env, name, func_args),
    }
}

fn call_function(
    ctx: &mut UtilContext<'_>,
    env: &mut BcEnv,
    name: &str,
    func_args: &[BcExpr],
) -> Result<f64, String> {
    // Compute arguments
    let mut arg_vals = Vec::new();
    for a in func_args {
        arg_vals.push(compute(ctx, env, a)?);
    }

    // Built-in functions
    match name {
        "sqrt" => {
            let v = arg_vals.first().copied().unwrap_or(0.0);
            if v < 0.0 {
                return Err("sqrt of negative number".to_string());
            }
            return Ok(v.sqrt());
        }
        "length" => {
            let v = arg_vals.first().copied().unwrap_or(0.0);
            let s = format!("{v}");
            let len = s.chars().filter(char::is_ascii_digit).count();
            return Ok(len as f64);
        }
        "scale" if !func_args.is_empty() => {
            let v = arg_vals.first().copied().unwrap_or(0.0);
            let s = format!("{v}");
            let scale = if let Some(pos) = s.find('.') {
                s.len() - pos - 1
            } else {
                0
            };
            return Ok(scale as f64);
        }
        _ => {}
    }

    // Math library functions
    if env.math_lib {
        match name {
            "s" => {
                let v = arg_vals.first().copied().unwrap_or(0.0);
                return Ok(v.sin());
            }
            "c" => {
                let v = arg_vals.first().copied().unwrap_or(0.0);
                return Ok(v.cos());
            }
            "a" => {
                let v = arg_vals.first().copied().unwrap_or(0.0);
                return Ok(v.atan());
            }
            "l" => {
                let v = arg_vals.first().copied().unwrap_or(0.0);
                if v <= 0.0 {
                    return Err("log of non-positive number".to_string());
                }
                return Ok(v.ln());
            }
            "e" => {
                let v = arg_vals.first().copied().unwrap_or(0.0);
                return Ok(v.exp());
            }
            _ => {}
        }
    }

    // User-defined functions
    let func = env.functions.iter().find(|(n, _, _)| n == name).cloned();
    if let Some((_, params, body)) = func {
        // Set parameters as local vars
        let saved: Vec<(String, f64)> =
            params.iter().map(|p| (p.clone(), env.get_var(p))).collect();
        for (i, param) in params.iter().enumerate() {
            let val = arg_vals.get(i).copied().unwrap_or(0.0);
            env.set_var(param, val);
        }
        let mut result = 0.0;
        for stmt in &body {
            if let BcStmt::Expr(ref e) = stmt {
                result = compute(ctx, env, e)?;
            } else {
                bc_run(ctx, env, stmt)?;
            }
        }
        // Restore saved vars
        for (vname, val) in saved {
            env.set_var(&vname, val);
        }
        return Ok(result);
    }

    Err(format!("undefined function: {name}"))
}

fn format_number(val: f64, scale: i32, obase: i32) -> String {
    if obase != 10 {
        // Output in custom base (integer only)
        let int_val = val as i64;
        return format_base(int_val, obase);
    }

    if scale <= 0 {
        // Integer output
        let truncated = val as i64;
        return truncated.to_string();
    }

    // Format with specified scale
    format!("{val:.prec$}", prec = scale as usize)
}

fn format_base(mut val: i64, base: i32) -> String {
    if val == 0 {
        return "0".to_string();
    }
    let negative = val < 0;
    if negative {
        val = -val;
    }
    let digits = "0123456789ABCDEF";
    let mut result = String::new();
    let base = base as i64;
    while val > 0 {
        let d = (val % base) as usize;
        result.push(digits.as_bytes()[d] as char);
        val /= base;
    }
    if negative {
        result.push('-');
    }
    result.chars().rev().collect()
}

fn i64_from_base(s: &str, base: i32) -> i64 {
    let base = base as i64;
    let mut result: i64 = 0;
    for ch in s.chars() {
        let d = match ch {
            '0'..='9' => (ch as i64) - ('0' as i64),
            'A'..='F' => (ch as i64) - ('A' as i64) + 10,
            'a'..='f' => (ch as i64) - ('a' as i64) + 10,
            _ => continue,
        };
        result = result * base + d;
    }
    result
}

fn bc_run(ctx: &mut UtilContext<'_>, env: &mut BcEnv, stmt: &BcStmt) -> Result<(), String> {
    match stmt {
        BcStmt::Expr(expr) => {
            let val = compute(ctx, env, expr)?;
            // Only print if it's a top-level expression (not an assignment)
            let should_print =
                !matches!(expr, BcExpr::Assign(_, _) | BcExpr::CompoundAssign(_, _, _));
            if should_print {
                let s = format!("{}\n", format_number(val, env.scale, env.obase));
                ctx.output.stdout(s.as_bytes());
            }
        }
        BcStmt::Print(expr) => {
            let val = compute(ctx, env, expr)?;
            let s = format_number(val, env.scale, env.obase);
            ctx.output.stdout(s.as_bytes());
        }
        BcStmt::If(cond, body) => {
            let val = compute(ctx, env, cond)?;
            if val != 0.0 {
                for s in body {
                    bc_run(ctx, env, s)?;
                }
            }
        }
        BcStmt::While(cond, body) => {
            let mut iterations = 0;
            loop {
                let val = compute(ctx, env, cond)?;
                if val == 0.0 {
                    break;
                }
                for s in body {
                    bc_run(ctx, env, s)?;
                }
                iterations += 1;
                if iterations > 100_000 {
                    return Err("infinite loop detected".to_string());
                }
            }
        }
        BcStmt::For(init, cond, incr, body) => {
            bc_run(ctx, env, init)?;
            let mut iterations = 0;
            loop {
                let val = compute(ctx, env, cond)?;
                if val == 0.0 {
                    break;
                }
                for s in body {
                    bc_run(ctx, env, s)?;
                }
                bc_run(ctx, env, incr)?;
                iterations += 1;
                if iterations > 100_000 {
                    return Err("infinite loop detected".to_string());
                }
            }
        }
        BcStmt::Quit => {}
        BcStmt::FnDef(name, params, body) => {
            env.functions
                .push((name.clone(), params.clone(), body.clone()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{UtilContext, VecOutput};
    use wasmsh_fs::MemoryFs;

    fn run_bc(input: &str, flags: &[&str]) -> (i32, String, String) {
        let mut fs = MemoryFs::new();
        let mut output = VecOutput::default();
        let input_bytes = input.as_bytes();
        let status = {
            let mut argv = vec!["bc"];
            argv.extend_from_slice(flags);
            let mut ctx = UtilContext {
                fs: &mut fs,
                output: &mut output,
                cwd: "/",
                stdin: Some(input_bytes),
                state: None,
            };
            util_bc(&mut ctx, &argv)
        };
        (
            status,
            output.stdout_str().to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )
    }

    #[test]
    fn bc_basic_arithmetic() {
        let (s, out, _) = run_bc("2 + 3\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "5");
    }

    #[test]
    fn bc_multiply() {
        let (_, out, _) = run_bc("6 * 7\n", &[]);
        assert_eq!(out.trim(), "42");
    }

    #[test]
    fn bc_divide_int() {
        let (_, out, _) = run_bc("10 / 3\n", &[]);
        assert_eq!(out.trim(), "3");
    }

    #[test]
    fn bc_scale() {
        let (_, out, _) = run_bc("scale = 4\n10 / 3\n", &[]);
        assert_eq!(out.trim(), "3.3333");
    }

    #[test]
    fn bc_variable() {
        let (_, out, _) = run_bc("x = 10\nx * 2\n", &[]);
        assert_eq!(out.trim(), "20");
    }

    #[test]
    fn bc_power() {
        let (_, out, _) = run_bc("2 ^ 10\n", &[]);
        assert_eq!(out.trim(), "1024");
    }

    #[test]
    fn bc_comparison() {
        let (_, out, _) = run_bc("3 < 5\n", &[]);
        assert_eq!(out.trim(), "1");
        let (_, out, _) = run_bc("5 < 3\n", &[]);
        assert_eq!(out.trim(), "0");
    }

    #[test]
    fn bc_if() {
        let (_, out, _) = run_bc("x = 5\nif (x > 3) { x * 2 }\n", &[]);
        assert_eq!(out.trim(), "10");
    }

    #[test]
    fn bc_while_loop() {
        let (_, out, _) = run_bc("x = 0\nwhile (x < 3) { x += 1 }\nx\n", &[]);
        assert_eq!(out.trim(), "3");
    }

    #[test]
    fn bc_for_loop() {
        let (_, out, _) = run_bc("s = 0\nfor (i = 1; i <= 5; i += 1) { s += i }\ns\n", &[]);
        assert_eq!(out.trim(), "15");
    }

    #[test]
    fn bc_sqrt() {
        let (_, out, _) = run_bc("sqrt(144)\n", &[]);
        assert_eq!(out.trim(), "12");
    }

    #[test]
    fn bc_math_lib_sin() {
        let (_, out, _) = run_bc("s(0)\n", &["-l"]);
        // sin(0) = 0
        let val: f64 = out.trim().parse().unwrap();
        assert!(val.abs() < 0.0001);
    }

    #[test]
    fn bc_division_by_zero() {
        let (status, _, err) = run_bc("1 / 0\n", &[]);
        assert_eq!(status, 1);
        assert!(err.contains("division by zero"));
    }

    #[test]
    fn bc_modulo() {
        let (_, out, _) = run_bc("17 % 5\n", &[]);
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn bc_negative() {
        let (_, out, _) = run_bc("-5 + 3\n", &[]);
        assert_eq!(out.trim(), "-2");
    }

    #[test]
    fn bc_multiple_statements() {
        let (_, out, _) = run_bc("1 + 1; 2 + 2\n", &[]);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "2");
        assert_eq!(lines[1], "4");
    }
}
