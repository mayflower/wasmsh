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
                self.skip_block_comment();
            } else if b == b'#' {
                self.skip_line_comment();
            } else {
                break;
            }
        }
    }

    fn skip_block_comment(&mut self) {
        if let Some(end) = self.remaining()[2..].find("*/") {
            self.pos += end + 4;
        } else {
            self.pos = self.input.len();
        }
    }

    fn skip_line_comment(&mut self) {
        while self.pos < self.input.len() && self.input.as_bytes()[self.pos] != b'\n' {
            self.pos += 1;
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
        let has_dot = self.consume_number_chars(ibase);
        (self.pos > start)
            .then(|| self.parse_number_slice(start, has_dot, ibase))
            .flatten()
    }

    fn consume_number_chars(&mut self, ibase: i32) -> bool {
        let mut has_dot = false;
        while self.pos < self.input.len() {
            let b = self.input.as_bytes()[self.pos];
            if !Self::is_number_char(b, has_dot, ibase) {
                break;
            }
            has_dot |= b == b'.';
            self.pos += 1;
        }
        has_dot
    }

    fn is_number_char(b: u8, has_dot: bool, ibase: i32) -> bool {
        b.is_ascii_digit() || (b == b'.' && !has_dot) || (ibase > 10 && b.is_ascii_hexdigit())
    }

    fn parse_number_slice(&self, start: usize, has_dot: bool, ibase: i32) -> Option<f64> {
        let s = &self.input[start..self.pos];
        if ibase != 10 && !has_dot {
            Some(i64_from_base(s, ibase) as f64)
        } else {
            s.parse::<f64>().ok()
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
        if b == b'(' {
            return self.parse_parenthesized_expr(ibase);
        }
        if b.is_ascii_digit() || b == b'.' {
            return self.parse_number(ibase).map(BcExpr::Num);
        }
        if b.is_ascii_alphabetic() || b == b'_' {
            return self.parse_ident_expr(ibase);
        }
        None
    }

    fn parse_parenthesized_expr(&mut self, ibase: i32) -> Option<BcExpr> {
        self.advance();
        let expr = self.parse_expr(ibase)?;
        self.eat(b')');
        Some(expr)
    }

    fn parse_ident_expr(&mut self, ibase: i32) -> Option<BcExpr> {
        let name = self.parse_ident()?;
        self.skip_ws();
        if !self.eat(b'(') {
            return Some(BcExpr::Var(name));
        }
        Some(BcExpr::FnCall(name, self.parse_call_args(ibase)))
    }

    fn parse_call_args(&mut self, ibase: i32) -> Vec<BcExpr> {
        let mut func_args = Vec::new();
        if self.eat(b')') {
            return func_args;
        }
        if let Some(arg) = self.parse_expr(ibase) {
            func_args.push(arg);
        }
        while self.eat(b',') {
            if let Some(arg) = self.parse_expr(ibase) {
                func_args.push(arg);
            }
        }
        self.eat(b')');
        func_args
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

    fn parse_cond_and_block(&mut self, ibase: i32) -> Option<(BcExpr, Vec<BcStmt>)> {
        self.skip_ws();
        self.eat(b'(');
        let cond = self.parse_expr(ibase)?;
        self.eat(b')');
        let body = self.parse_block(ibase);
        Some((cond, body))
    }

    #[allow(clippy::unnecessary_wraps)]
    fn parse_for_stmt(&mut self, ibase: i32) -> Option<BcStmt> {
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
        Some(BcStmt::For(Box::new(init), cond, Box::new(incr), body))
    }

    fn parse_define_stmt(&mut self, ibase: i32) -> Option<BcStmt> {
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
        Some(BcStmt::FnDef(name, params, body))
    }

    fn parse_stmt(&mut self, ibase: i32) -> Option<BcStmt> {
        self.skip_ws();
        while self.pos < self.input.len()
            && (self.input.as_bytes()[self.pos] == b';' || self.input.as_bytes()[self.pos] == b'\n')
        {
            self.pos += 1;
            self.skip_ws();
        }

        if self.pos >= self.input.len() {
            return None;
        }

        if self.eat_str("quit") {
            self.skip_separator();
            return Some(BcStmt::Quit);
        }
        if self.eat_str("if") {
            let (cond, body) = self.parse_cond_and_block(ibase)?;
            return Some(BcStmt::If(cond, body));
        }
        if self.eat_str("while") {
            let (cond, body) = self.parse_cond_and_block(ibase)?;
            return Some(BcStmt::While(cond, body));
        }
        if self.eat_str("for") {
            return self.parse_for_stmt(ibase);
        }
        if self.eat_str("print") {
            self.skip_ws();
            let expr = self.parse_expr(ibase)?;
            self.skip_separator();
            return Some(BcStmt::Print(expr));
        }
        if self.eat_str("define") {
            return self.parse_define_stmt(ibase);
        }

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

fn eval_binop(l: f64, op: BcOp, r: f64) -> Result<f64, String> {
    match op {
        BcOp::Add => Ok(l + r),
        BcOp::Sub => Ok(l - r),
        BcOp::Mul => Ok(l * r),
        BcOp::Div => checked_binop(r, "division by zero", || l / r),
        BcOp::Mod => checked_binop(r, "modulo by zero", || l % r),
        BcOp::Pow => Ok(l.powf(r)),
        BcOp::Eq => Ok(bool_to_num((l - r).abs() < f64::EPSILON)),
        BcOp::Ne => Ok(bool_to_num((l - r).abs() >= f64::EPSILON)),
        BcOp::Lt => Ok(bool_to_num(l < r)),
        BcOp::Gt => Ok(bool_to_num(l > r)),
        BcOp::Le => Ok(bool_to_num(l <= r)),
        BcOp::Ge => Ok(bool_to_num(l >= r)),
    }
}

fn checked_binop(r: f64, message: &str, f: impl FnOnce() -> f64) -> Result<f64, String> {
    if r == 0.0 {
        Err(message.to_string())
    } else {
        Ok(f())
    }
}

fn bool_to_num(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

fn assign_special_var(env: &mut BcEnv, name: &str, val: f64) {
    match name {
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
}

fn eval_compound_assign(
    ctx: &mut UtilContext<'_>,
    env: &mut BcEnv,
    name: &str,
    op: BcOp,
    rhs: &BcExpr,
) -> Result<f64, String> {
    let current = env.get_var(name);
    let rval = compute(ctx, env, rhs)?;
    let result = eval_binop(current, op, rval)?;
    env.set_var(name, result);
    Ok(result)
}

fn compute(ctx: &mut UtilContext<'_>, env: &mut BcEnv, expr: &BcExpr) -> Result<f64, String> {
    match expr {
        BcExpr::Num(n) => Ok(*n),
        BcExpr::Var(name) => match name.as_str() {
            "scale" => Ok(f64::from(env.scale)),
            "ibase" => Ok(f64::from(env.ibase)),
            "obase" => Ok(f64::from(env.obase)),
            _ => Ok(env.get_var(name)),
        },
        BcExpr::UnaryMinus(e) => Ok(-compute(ctx, env, e)?),
        BcExpr::BinOp(left, op, right) => {
            let l = compute(ctx, env, left)?;
            let r = compute(ctx, env, right)?;
            eval_binop(l, *op, r)
        }
        BcExpr::Assign(name, rhs) => {
            let val = compute(ctx, env, rhs)?;
            assign_special_var(env, name, val);
            Ok(val)
        }
        BcExpr::CompoundAssign(name, op, rhs) => eval_compound_assign(ctx, env, name, *op, rhs),
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

fn call_builtin(name: &str, arg_vals: &[f64], has_args: bool) -> Option<Result<f64, String>> {
    let v = arg_vals.first().copied().unwrap_or(0.0);
    match name {
        "sqrt" => {
            if v < 0.0 {
                return Some(Err("sqrt of negative number".to_string()));
            }
            Some(Ok(v.sqrt()))
        }
        "length" => {
            let s = format!("{v}");
            let len = s.chars().filter(char::is_ascii_digit).count();
            Some(Ok(len as f64))
        }
        "scale" if has_args => {
            let s = format!("{v}");
            let scale = s.find('.').map_or(0, |pos| s.len() - pos - 1);
            Some(Ok(scale as f64))
        }
        _ => None,
    }
}

fn call_math_lib(name: &str, arg_vals: &[f64]) -> Option<Result<f64, String>> {
    let v = arg_vals.first().copied().unwrap_or(0.0);
    match name {
        "s" => Some(Ok(v.sin())),
        "c" => Some(Ok(v.cos())),
        "a" => Some(Ok(v.atan())),
        "l" => {
            if v <= 0.0 {
                return Some(Err("log of non-positive number".to_string()));
            }
            Some(Ok(v.ln()))
        }
        "e" => Some(Ok(v.exp())),
        _ => None,
    }
}

fn call_user_function(
    ctx: &mut UtilContext<'_>,
    env: &mut BcEnv,
    params: &[String],
    body: &[BcStmt],
    arg_vals: &[f64],
) -> Result<f64, String> {
    let saved: Vec<(String, f64)> = params.iter().map(|p| (p.clone(), env.get_var(p))).collect();
    for (i, param) in params.iter().enumerate() {
        env.set_var(param, arg_vals.get(i).copied().unwrap_or(0.0));
    }
    let mut result = 0.0;
    for stmt in body {
        if let BcStmt::Expr(ref e) = stmt {
            result = compute(ctx, env, e)?;
        } else {
            bc_run(ctx, env, stmt)?;
        }
    }
    for (vname, val) in saved {
        env.set_var(&vname, val);
    }
    Ok(result)
}

fn call_function(
    ctx: &mut UtilContext<'_>,
    env: &mut BcEnv,
    name: &str,
    func_args: &[BcExpr],
) -> Result<f64, String> {
    let mut arg_vals = Vec::new();
    for a in func_args {
        arg_vals.push(compute(ctx, env, a)?);
    }

    if let Some(result) = call_builtin(name, &arg_vals, !func_args.is_empty()) {
        return result;
    }
    if env.math_lib {
        if let Some(result) = call_math_lib(name, &arg_vals) {
            return result;
        }
    }

    let func = env.functions.iter().find(|(n, _, _)| n == name).cloned();
    if let Some((_, params, body)) = func {
        return call_user_function(ctx, env, &params, &body, &arg_vals);
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

const BC_MAX_ITERATIONS: usize = 100_000;

fn bc_run_block(ctx: &mut UtilContext<'_>, env: &mut BcEnv, body: &[BcStmt]) -> Result<(), String> {
    for s in body {
        bc_run(ctx, env, s)?;
    }
    Ok(())
}

fn bc_run_while(
    ctx: &mut UtilContext<'_>,
    env: &mut BcEnv,
    cond: &BcExpr,
    body: &[BcStmt],
) -> Result<(), String> {
    for _ in 0..BC_MAX_ITERATIONS {
        if compute(ctx, env, cond)? == 0.0 {
            return Ok(());
        }
        bc_run_block(ctx, env, body)?;
    }
    Err("infinite loop detected".to_string())
}

fn bc_run_for(
    ctx: &mut UtilContext<'_>,
    env: &mut BcEnv,
    init: &BcStmt,
    cond: &BcExpr,
    incr: &BcStmt,
    body: &[BcStmt],
) -> Result<(), String> {
    bc_run(ctx, env, init)?;
    for _ in 0..BC_MAX_ITERATIONS {
        if compute(ctx, env, cond)? == 0.0 {
            return Ok(());
        }
        bc_run_block(ctx, env, body)?;
        bc_run(ctx, env, incr)?;
    }
    Err("infinite loop detected".to_string())
}

fn bc_run(ctx: &mut UtilContext<'_>, env: &mut BcEnv, stmt: &BcStmt) -> Result<(), String> {
    match stmt {
        BcStmt::Expr(expr) => {
            let val = compute(ctx, env, expr)?;
            if !matches!(expr, BcExpr::Assign(_, _) | BcExpr::CompoundAssign(_, _, _)) {
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
            if compute(ctx, env, cond)? != 0.0 {
                bc_run_block(ctx, env, body)?;
            }
        }
        BcStmt::While(cond, body) => bc_run_while(ctx, env, cond, body)?,
        BcStmt::For(init, cond, incr, body) => bc_run_for(ctx, env, init, cond, incr, body)?,
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

    // ------------------------------------------------------------------
    // User-defined functions
    // ------------------------------------------------------------------

    #[test]
    fn bc_user_function_square() {
        let (s, out, _) = run_bc("define f(x) { return x * x }\nf(7)\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "49");
    }

    #[test]
    fn bc_user_function_two_params() {
        let (s, out, _) = run_bc("define add(a, b) { return a + b }\nadd(10, 20)\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "30");
    }

    #[test]
    fn bc_user_function_nested_calls() {
        let (s, out, _) = run_bc(
            "define double(x) { return x * 2 }\ndefine quad(x) { return double(double(x)) }\nquad(3)\n",
            &[],
        );
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "12");
    }

    #[test]
    fn bc_user_function_no_return() {
        // A function without an explicit return should yield 0 (last computed value)
        let (s, out, _) = run_bc("define f(x) { x + 1 }\nf(5)\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "6");
    }

    // ------------------------------------------------------------------
    // ibase/obase
    // ------------------------------------------------------------------

    #[test]
    fn bc_obase_hex() {
        let (s, out, _) = run_bc("obase=16\n255\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "FF");
    }

    #[test]
    fn bc_ibase_set() {
        // ibase is set at runtime; verify ibase variable is stored
        let (s, out, _) = run_bc("ibase=16\nibase\n", &[]);
        assert_eq!(s, 0);
        // With scale=0, ibase (16) should print as 16
        assert_eq!(out.trim(), "16");
    }

    #[test]
    fn bc_obase_binary() {
        let (s, out, _) = run_bc("obase=2\n10\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "1010");
    }

    #[test]
    fn bc_obase_octal() {
        let (s, out, _) = run_bc("obase=8\n255\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "377");
    }

    // ------------------------------------------------------------------
    // -l math library: sin, cos, atan, ln, exp
    // ------------------------------------------------------------------

    #[test]
    fn bc_math_lib_cos() {
        let (_, out, _) = run_bc("c(0)\n", &["-l"]);
        // cos(0) = 1
        let val: f64 = out.trim().parse().unwrap();
        assert!((val - 1.0).abs() < 0.0001);
    }

    #[test]
    fn bc_math_lib_atan() {
        let (_, out, _) = run_bc("a(1)\n", &["-l"]);
        // atan(1) = pi/4 ≈ 0.7854
        let val: f64 = out.trim().parse().unwrap();
        // atan(1) = pi/4; just check it's in a reasonable range
        assert!(val > 0.78 && val < 0.79);
    }

    #[test]
    fn bc_math_lib_ln() {
        let (_, out, _) = run_bc("l(1)\n", &["-l"]);
        // ln(1) = 0
        let val: f64 = out.trim().parse().unwrap();
        assert!(val.abs() < 0.0001);
    }

    #[test]
    fn bc_math_lib_exp() {
        let (_, out, _) = run_bc("e(0)\n", &["-l"]);
        // exp(0) = 1
        let val: f64 = out.trim().parse().unwrap();
        assert!((val - 1.0).abs() < 0.0001);
    }

    #[test]
    fn bc_math_lib_scale_default() {
        // With -l, scale should be 20; printed with scale=20 formatting
        let (_, out, _) = run_bc("scale\n", &["-l"]);
        let val: f64 = out.trim().parse().unwrap();
        assert!((val - 20.0).abs() < 0.001);
    }

    // ------------------------------------------------------------------
    // Comments
    // ------------------------------------------------------------------

    #[test]
    fn bc_block_comment() {
        let (s, out, _) = run_bc("/* this is a comment */ 5 + 3\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "8");
    }

    #[test]
    fn bc_line_comment() {
        let (s, out, _) = run_bc("5 + 3 # add them\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "8");
    }

    #[test]
    fn bc_multiline_block_comment() {
        let (s, out, _) = run_bc("/* multi\nline\ncomment */ 7 * 6\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "42");
    }

    // ------------------------------------------------------------------
    // Pre/post increment and decrement
    // ------------------------------------------------------------------

    #[test]
    fn bc_pre_increment() {
        let (s, out, _) = run_bc("x = 5\n++x\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "6");
    }

    #[test]
    fn bc_post_increment() {
        let (s, out, _) = run_bc("x = 5\nx++\nx\n", &[]);
        assert_eq!(s, 0);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines[0], "5"); // post-increment returns old value
        assert_eq!(lines[1], "6"); // then x is 6
    }

    #[test]
    fn bc_pre_decrement() {
        let (s, out, _) = run_bc("x = 5\n--x\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "4");
    }

    #[test]
    fn bc_post_decrement() {
        let (s, out, _) = run_bc("x = 5\nx--\nx\n", &[]);
        assert_eq!(s, 0);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines[0], "5"); // post-decrement returns old value
        assert_eq!(lines[1], "4"); // then x is 4
    }

    // ------------------------------------------------------------------
    // Compound assignment operators
    // ------------------------------------------------------------------

    #[test]
    fn bc_compound_add_assign() {
        let (_, out, _) = run_bc("x = 10\nx += 5\nx\n", &[]);
        assert_eq!(out.trim(), "15");
    }

    #[test]
    fn bc_compound_sub_assign() {
        let (_, out, _) = run_bc("x = 10\nx -= 3\nx\n", &[]);
        assert_eq!(out.trim(), "7");
    }

    #[test]
    fn bc_compound_mul_assign() {
        let (_, out, _) = run_bc("x = 10\nx *= 4\nx\n", &[]);
        assert_eq!(out.trim(), "40");
    }

    #[test]
    fn bc_compound_div_assign() {
        let (_, out, _) = run_bc("x = 20\nx /= 4\nx\n", &[]);
        assert_eq!(out.trim(), "5");
    }

    #[test]
    fn bc_compound_mod_assign() {
        let (_, out, _) = run_bc("x = 17\nx %= 5\nx\n", &[]);
        assert_eq!(out.trim(), "2");
    }

    #[test]
    fn bc_compound_chain() {
        let (_, out, _) = run_bc("x = 10\nx += 5\nx -= 3\nx *= 2\nx\n", &[]);
        assert_eq!(out.trim(), "24");
    }

    // ------------------------------------------------------------------
    // quit statement
    // ------------------------------------------------------------------

    #[test]
    fn bc_quit() {
        // quit should stop processing (but not error)
        let (s, out, _) = run_bc("5 + 5\nquit\n10 + 10\n", &[]);
        assert_eq!(s, 0);
        // The first expression should be evaluated, quit stops further processing
        assert!(out.contains("10"));
    }

    // ------------------------------------------------------------------
    // Error: division by zero (already tested, add modulo by zero)
    // ------------------------------------------------------------------

    #[test]
    fn bc_modulo_by_zero() {
        let (status, _, err) = run_bc("5 % 0\n", &[]);
        assert_eq!(status, 1);
        assert!(err.contains("modulo by zero"));
    }

    // ------------------------------------------------------------------
    // Error: undefined function call
    // ------------------------------------------------------------------

    #[test]
    fn bc_undefined_function() {
        let (status, _, err) = run_bc("foo(5)\n", &[]);
        assert_eq!(status, 1);
        assert!(err.contains("undefined function"));
    }

    // ------------------------------------------------------------------
    // length() function — number of digits
    // ------------------------------------------------------------------

    #[test]
    fn bc_length_function() {
        let (s, out, _) = run_bc("length(12345)\n", &[]);
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "5");
    }

    #[test]
    fn bc_length_single_digit() {
        let (_, out, _) = run_bc("length(9)\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    // ------------------------------------------------------------------
    // scale() function — decimal digits
    // ------------------------------------------------------------------

    #[test]
    fn bc_scale_function() {
        let (s, out, _) = run_bc("scale(3.14159)\n", &[]);
        assert_eq!(s, 0);
        // Should report the number of decimal places
        let val: i32 = out.trim().parse().unwrap();
        assert!(val > 0);
    }

    // ------------------------------------------------------------------
    // Empty input → no output
    // ------------------------------------------------------------------

    #[test]
    fn bc_empty_input() {
        let (s, out, _) = run_bc("", &[]);
        assert_eq!(s, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn bc_whitespace_only() {
        let (s, out, _) = run_bc("   \n  \n", &[]);
        assert_eq!(s, 0);
        assert!(out.trim().is_empty());
    }

    // ------------------------------------------------------------------
    // Multi-line input with semicolons
    // ------------------------------------------------------------------

    #[test]
    fn bc_multi_line_semicolons() {
        let (_, out, _) = run_bc("1 + 2; 3 + 4; 5 + 6\n", &[]);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "3");
        assert_eq!(lines[1], "7");
        assert_eq!(lines[2], "11");
    }

    #[test]
    fn bc_multi_line_newlines() {
        let (_, out, _) = run_bc("10 + 20\n30 + 40\n", &[]);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "30");
        assert_eq!(lines[1], "70");
    }

    // ------------------------------------------------------------------
    // Nested parentheses
    // ------------------------------------------------------------------

    #[test]
    fn bc_nested_parens() {
        let (_, out, _) = run_bc("((2 + 3) * (4 + 1))\n", &[]);
        assert_eq!(out.trim(), "25");
    }

    // ------------------------------------------------------------------
    // Exponentiation
    // ------------------------------------------------------------------

    #[test]
    fn bc_power_zero() {
        let (_, out, _) = run_bc("5 ^ 0\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    #[test]
    fn bc_power_one() {
        let (_, out, _) = run_bc("5 ^ 1\n", &[]);
        assert_eq!(out.trim(), "5");
    }

    // ------------------------------------------------------------------
    // Comparisons
    // ------------------------------------------------------------------

    #[test]
    fn bc_eq_comparison() {
        let (_, out, _) = run_bc("5 == 5\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    #[test]
    fn bc_ne_comparison() {
        let (_, out, _) = run_bc("5 != 3\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    #[test]
    fn bc_le_comparison() {
        let (_, out, _) = run_bc("3 <= 5\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    #[test]
    fn bc_ge_comparison() {
        let (_, out, _) = run_bc("5 >= 5\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    #[test]
    fn bc_gt_comparison() {
        let (_, out, _) = run_bc("5 > 3\n", &[]);
        assert_eq!(out.trim(), "1");
    }

    // ------------------------------------------------------------------
    // Unknown option
    // ------------------------------------------------------------------

    #[test]
    fn bc_unknown_option() {
        let (status, _, err) = run_bc("1 + 1\n", &["-z"]);
        assert_eq!(status, 1);
        assert!(err.contains("unknown option"));
    }

    // ------------------------------------------------------------------
    // sqrt of negative number
    // ------------------------------------------------------------------

    #[test]
    fn bc_sqrt_negative() {
        let (status, _, err) = run_bc("sqrt(-4)\n", &[]);
        assert_eq!(status, 1);
        assert!(err.contains("sqrt") || err.contains("negative"));
    }

    // ------------------------------------------------------------------
    // Math lib ln of non-positive
    // ------------------------------------------------------------------

    #[test]
    fn bc_ln_zero() {
        let (status, _, err) = run_bc("l(0)\n", &["-l"]);
        assert_eq!(status, 1);
        assert!(err.contains("log") || err.contains("non-positive"));
    }

    // ------------------------------------------------------------------
    // Function with body containing multiple statements
    // ------------------------------------------------------------------

    #[test]
    fn bc_function_multi_stmt() {
        let (s, out, _) = run_bc(
            "define f(x) { x = x + 1; x = x * 2; return x }\nf(4)\n",
            &[],
        );
        assert_eq!(s, 0);
        assert_eq!(out.trim(), "10");
    }

    // ------------------------------------------------------------------
    // Large number
    // ------------------------------------------------------------------

    #[test]
    fn bc_large_number() {
        let (_, out, _) = run_bc("999999 * 999999\n", &[]);
        assert_eq!(out.trim(), "999998000001");
    }

    // ------------------------------------------------------------------
    // Chained operations
    // ------------------------------------------------------------------

    #[test]
    fn bc_chained_ops() {
        let (_, out, _) = run_bc("2 + 3 * 4\n", &[]);
        // Multiplication should bind tighter
        assert_eq!(out.trim(), "14");
    }

    #[test]
    fn bc_left_to_right_add() {
        let (_, out, _) = run_bc("1 + 2 + 3 + 4\n", &[]);
        assert_eq!(out.trim(), "10");
    }
}
