//! Converts the AST back to PHP code, exactly like `zend_ast_export()`
//! (`Zend/zend_ast.c`). PHP uses this for `assert()` failure messages
//! (`assert($a > 1)`), so the output is byte-for-byte that of PHP: operator
//! precedence decides parenthesization, strings are re-quoted, interpolated
//! variables are wrapped in braces, and so on.

use crate::ast::{
    Arg, ArgKind, ArgList, ArrayItem, ArrowFn, AttributeGroup, BinaryOp, Callee, CastKind,
    ClassConstGroup, ClassDecl, ClassKind, ClassMember, ClassMemberKind, ClassRef, Closure,
    ConstElem, Expr, ExprKind, HookBody, IncDecOp, IncludeKind, InterpPart, MatchArm, MemberName,
    MethodDecl, Modifiers, Name, NameKind, NewClass, Param, Program, PropertyGroup, PropertyHook,
    Stmt, StmtKind, TraitAdaptation, TraitMethodRef, Type, TypeKind, UnaryOp, UseItem, UseKind,
    VarName,
};

/// Exports an expression (`zend_ast_export("", ast, "")`).
pub fn export_expr(expr: &Expr) -> Vec<u8> {
    let mut e = Exporter { out: Vec::new() };
    e.expr(expr, 0, 0);
    e.out
}

/// Exports a statement as it appears inside a statement list (indented
/// and terminated, like `zend_ast_export_stmt` at indent 0).
pub fn export_stmt(stmt: &Stmt) -> Vec<u8> {
    let mut e = Exporter { out: Vec::new() };
    e.stmt(stmt, 0);
    e.out
}

/// Exports all statements of a program.
pub fn export_program(program: &Program) -> Vec<u8> {
    let mut e = Exporter { out: Vec::new() };
    e.stmts(&program.statements, 0);
    e.out
}

/// Exports a call argument (`assertion: false`, `...$a`, `$x`).
pub fn export_arg(arg: &Arg) -> Vec<u8> {
    let mut e = Exporter { out: Vec::new() };
    e.arg(arg, 0);
    e.out
}

/// The message of a failed `assert(...)` whose first argument is `arg`:
/// `assert(<export of the argument>)` (as `zend_compile_assert` builds it).
pub fn assert_message(arg: &Arg) -> Vec<u8> {
    let mut out = b"assert(".to_vec();
    let mut e = Exporter { out: Vec::new() };
    // `zend_ast_export` starts at priority 0.
    match &arg.kind {
        ArgKind::Positional(x) => e.expr(x, 0, 0),
        _ => e.arg(arg, 0),
    }
    out.extend_from_slice(&e.out);
    out.push(b')');
    out
}

/// Formats a float like `smart_str_append_double(.., precision, true)`
/// (`zend_gcvt` with `%G`-style output and a `.0` for integral values).
pub fn format_double(value: f64, precision: usize) -> String {
    if value.is_nan() {
        return "NAN".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-INF" } else { "INF" }.to_owned();
    }
    let precision = precision.max(1);
    let mut out = String::new();
    if value.is_sign_negative() {
        out.push('-');
    }
    let v = value.abs();
    let (digits, decpt) = if v == 0.0 {
        ("0".to_owned(), 1i32)
    } else {
        let s = format!("{:.*e}", precision - 1, v);
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let mut digits: String = mant.chars().filter(char::is_ascii_digit).collect();
        while digits.len() > 1 && digits.ends_with('0') {
            digits.pop();
        }
        (digits, exp + 1)
    };
    #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
    let ndigit = precision as i32;
    if decpt < -3 || decpt > ndigit {
        // exponential format
        let e = decpt - 1;
        let bytes = digits.as_bytes();
        out.push(bytes[0] as char);
        out.push('.');
        if bytes.len() == 1 {
            out.push('0');
        } else {
            out.push_str(&digits[1..]);
        }
        out.push('E');
        out.push(if e < 0 { '-' } else { '+' });
        out.push_str(&e.abs().to_string());
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in decpt..0 {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        #[allow(clippy::cast_sign_loss)]
        let decpt = decpt as usize;
        if digits.len() <= decpt {
            out.push_str(&digits);
            for _ in digits.len()..decpt {
                out.push('0');
            }
            out.push_str(".0");
        } else {
            out.push_str(&digits[..decpt]);
            out.push('.');
            out.push_str(&digits[decpt..]);
        }
    }
    out
}

struct Exporter {
    out: Vec<u8>,
}

fn binary_prec(op: BinaryOp) -> (&'static str, i32, i32, i32) {
    match op {
        BinaryOp::Add => (" + ", 200, 200, 201),
        BinaryOp::Sub => (" - ", 200, 200, 201),
        BinaryOp::Mul => (" * ", 210, 210, 211),
        BinaryOp::Div => (" / ", 210, 210, 211),
        BinaryOp::Mod => (" % ", 210, 210, 211),
        BinaryOp::ShiftLeft => (" << ", 190, 190, 191),
        BinaryOp::ShiftRight => (" >> ", 190, 190, 191),
        BinaryOp::Concat => (" . ", 185, 185, 186),
        BinaryOp::BitOr => (" | ", 140, 140, 141),
        BinaryOp::BitAnd => (" & ", 160, 160, 161),
        BinaryOp::BitXor => (" ^ ", 150, 150, 151),
        BinaryOp::Identical => (" === ", 170, 171, 171),
        BinaryOp::NotIdentical => (" !== ", 170, 171, 171),
        BinaryOp::Equal => (" == ", 170, 171, 171),
        BinaryOp::NotEqual => (" != ", 170, 171, 171),
        BinaryOp::Less => (" < ", 180, 181, 181),
        BinaryOp::LessEqual => (" <= ", 180, 181, 181),
        BinaryOp::Greater => (" > ", 180, 181, 181),
        BinaryOp::GreaterEqual => (" >= ", 180, 181, 181),
        BinaryOp::Spaceship => (" <=> ", 180, 181, 181),
        BinaryOp::Pow => (" ** ", 250, 251, 250),
        BinaryOp::BoolXor => (" xor ", 40, 40, 41),
        BinaryOp::BoolAnd => (" && ", 130, 130, 131),
        BinaryOp::BoolOr => (" || ", 120, 120, 121),
        BinaryOp::Coalesce => (" ?? ", 110, 111, 110),
    }
}

fn assign_op_str(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => " += ",
        BinaryOp::Sub => " -= ",
        BinaryOp::Mul => " *= ",
        BinaryOp::Div => " /= ",
        BinaryOp::Mod => " %= ",
        BinaryOp::ShiftLeft => " <<= ",
        BinaryOp::ShiftRight => " >>= ",
        BinaryOp::Concat => " .= ",
        BinaryOp::BitOr => " |= ",
        BinaryOp::BitAnd => " &= ",
        BinaryOp::BitXor => " ^= ",
        BinaryOp::Pow => " **= ",
        BinaryOp::Coalesce => " ??= ",
        _ => " = ",
    }
}

/// `zend_ast_valid_var_name`
fn valid_var_name(s: &[u8]) -> bool {
    let Some(&c) = s.first() else {
        return false;
    };
    let ok_start = |c: u8| c == b'_' || c >= 127 || c.is_ascii_alphabetic();
    if !ok_start(c) {
        return false;
    }
    s[1..].iter().all(|&c| ok_start(c) || c.is_ascii_digit())
}

impl Exporter {
    fn s(&mut self, s: &str) {
        self.out.extend_from_slice(s.as_bytes());
    }

    fn b(&mut self, s: &[u8]) {
        self.out.extend_from_slice(s);
    }

    fn c(&mut self, c: u8) {
        self.out.push(c);
    }

    fn indent(&mut self, n: usize) {
        for _ in 0..n {
            self.s("    ");
        }
    }

    /// `zend_ast_export_str`
    fn str_escaped(&mut self, s: &[u8]) {
        for &c in s {
            if c == b'\'' || c == b'\\' {
                self.c(b'\\');
            }
            self.c(c);
        }
    }

    /// `zend_ast_export_qstr`
    fn qstr(&mut self, quote: u8, s: &[u8]) {
        for &c in s {
            if c < b' ' {
                match c {
                    b'\n' => self.s("\\n"),
                    b'\r' => self.s("\\r"),
                    b'\t' => self.s("\\t"),
                    0x0c => self.s("\\f"),
                    0x0b => self.s("\\v"),
                    0x1b => self.s("\\e"),
                    _ => {
                        self.s("\\0");
                        self.c(b'0' + c / 8);
                        self.c(b'0' + c % 8);
                    }
                }
            } else {
                if c == quote || c == b'$' || c == b'\\' {
                    self.c(b'\\');
                }
                self.c(c);
            }
        }
    }

    /// `zend_ast_export_quoted_str`
    fn quoted_str(&mut self, s: &[u8]) {
        if s.iter().any(|&c| c < b' ') {
            self.c(b'"');
            self.qstr(b'"', s);
            self.c(b'"');
        } else {
            self.c(b'\'');
            self.str_escaped(s);
            self.c(b'\'');
        }
    }

    fn ns_name(&mut self, n: &Name) {
        match n.kind {
            NameKind::FullyQualified => self.c(b'\\'),
            NameKind::Relative => self.s("namespace\\"),
            NameKind::Unqualified | NameKind::Qualified => {}
        }
        self.b(&n.name);
    }

    /// `zend_ast_export_ns_name_or_expression`
    fn ns_name_or_expr(&mut self, e: &Expr, indent: usize) {
        if let ExprKind::String(s) = &e.kind {
            // A string literal zval has attr 0, which is `ZEND_NAME_FQ`.
            self.c(b'\\');
            self.b(s);
        } else if is_zval(e) || matches!(e.kind, ExprKind::Var(_)) {
            self.expr(e, 0, indent);
        } else {
            self.c(b'(');
            self.expr(e, 0, indent);
            self.c(b')');
        }
    }

    fn class_ref(&mut self, c: &ClassRef, indent: usize) {
        match c {
            ClassRef::Name(n) => self.ns_name(n),
            ClassRef::Expr(e) => self.ns_name_or_expr(e, indent),
        }
    }

    /// `zend_ast_export_name` for an expression node.
    fn name_expr(&mut self, e: &Expr, indent: usize) {
        if let ExprKind::String(s) = &e.kind {
            self.b(s);
        } else {
            self.expr(e, 0, indent);
        }
    }

    /// `zend_ast_export_var` for an identifier.
    fn var_ident(&mut self, name: &[u8]) {
        if valid_var_name(name) {
            self.b(name);
        } else {
            self.s("{'");
            self.str_escaped(name);
            self.s("'}");
        }
    }

    /// `zend_ast_export_var` for an expression node.
    fn var_expr(&mut self, e: &Expr, indent: usize) {
        match &e.kind {
            ExprKind::String(s) => self.var_ident(s),
            ExprKind::Var(_) => self.expr(e, 0, indent),
            _ => {
                self.c(b'{');
                self.name_expr(e, indent);
                self.c(b'}');
            }
        }
    }

    fn var_name(&mut self, v: &VarName, indent: usize) {
        match v {
            VarName::Name(id) => self.var_ident(&id.name),
            VarName::Expr(e) => self.var_expr(e, indent),
        }
    }

    fn member_name(&mut self, m: &MemberName, indent: usize) {
        match m {
            MemberName::Ident(id) => self.var_ident(&id.name),
            MemberName::Expr(e) => self.var_expr(e, indent),
        }
    }

    fn args(&mut self, args: &ArgList, indent: usize) {
        for (i, a) in args.args.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            self.arg(a, indent);
        }
    }

    fn arg(&mut self, a: &Arg, indent: usize) {
        match &a.kind {
            ArgKind::Positional(e) => self.expr(e, 20, indent),
            ArgKind::Unpack(e) => {
                self.s("...");
                self.expr(e, 20, indent);
            }
            ArgKind::Named { name, value } => {
                self.b(&name.name);
                self.s(": ");
                self.expr(value, 20, indent);
            }
            ArgKind::Placeholder => self.c(b'?'),
            ArgKind::VariadicPlaceholder => self.s("..."),
            ArgKind::NamedPlaceholder(name) => {
                self.b(&name.name);
                self.s(": ?");
            }
        }
    }

    fn array_item(&mut self, item: &ArrayItem, indent: usize) {
        if item.unpack {
            self.s("...");
            self.expr(&item.value, 20, indent);
            return;
        }
        if let Some(k) = &item.key {
            self.expr(k, 80, indent);
            self.s(" => ");
        }
        if item.by_ref {
            self.c(b'&');
        }
        self.expr(&item.value, 80, indent);
    }

    fn encaps(&mut self, quote: u8, parts: &[InterpPart], indent: usize) {
        for p in parts {
            match p {
                InterpPart::Literal(s, _) => self.qstr(quote, s),
                InterpPart::Expr { expr, .. } => {
                    self.c(b'{');
                    self.expr(expr, 0, indent);
                    self.c(b'}');
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)] // mirrors the `BINARY_OP` macro
    fn binary(
        &mut self,
        op: &str,
        p: i32,
        pl: i32,
        pr: i32,
        l: &Expr,
        r: &Expr,
        prio: i32,
        indent: usize,
    ) {
        if prio > p {
            self.c(b'(');
        }
        self.expr(l, pl, indent);
        self.s(op);
        self.expr(r, pr, indent);
        if prio > p {
            self.c(b')');
        }
    }

    fn prefix(&mut self, op: &str, p: i32, pl: i32, e: &Expr, prio: i32, indent: usize) {
        if prio > p {
            self.c(b'(');
        }
        self.s(op);
        self.expr(e, pl, indent);
        if prio > p {
            self.c(b')');
        }
    }

    fn func_op(&mut self, op: &str, e: &Expr, indent: usize) {
        self.s(op);
        self.c(b'(');
        self.expr(e, 0, indent);
        self.c(b')');
    }

    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, e: &Expr, prio: i32, indent: usize) {
        match &e.kind {
            ExprKind::Int(i) => self.s(&i.to_string()),
            ExprKind::Float(f) => self.s(&format_double(*f, 14)),
            ExprKind::String(s) => self.quoted_str(s),
            ExprKind::Interpolated(parts) => {
                self.c(b'"');
                self.encaps(b'"', parts, indent);
                self.c(b'"');
            }
            ExprKind::ShellExec(parts) => {
                self.c(b'`');
                self.encaps(b'`', parts, indent);
                self.c(b'`');
            }
            ExprKind::Array { items, .. } => {
                self.c(b'[');
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        self.s(", ");
                    }
                    if let Some(it) = it {
                        self.array_item(it, indent);
                    }
                }
                self.c(b']');
            }
            ExprKind::Const(n) => self.ns_name(n),
            ExprKind::MagicConst(m) => self.s(m.as_str()),
            ExprKind::ClassConst { class, name } => {
                self.class_ref(class, indent);
                self.s("::");
                match name {
                    MemberName::Ident(id) => self.b(&id.name),
                    MemberName::Expr(x) => self.name_expr(x, indent),
                }
            }
            ExprKind::ClassName(class) => {
                self.class_ref(class, indent);
                self.s("::class");
            }
            ExprKind::Var(v) => {
                self.c(b'$');
                self.var_name(v, indent);
            }
            ExprKind::Dim { base, index } => {
                self.expr(base, 260, indent);
                self.c(b'[');
                if let Some(i) = index {
                    self.expr(i, 0, indent);
                }
                self.c(b']');
            }
            ExprKind::Prop {
                object,
                name,
                nullsafe,
            } => {
                self.expr(object, 0, indent);
                self.s(if *nullsafe { "?->" } else { "->" });
                self.member_name(name, indent);
            }
            ExprKind::StaticProp { class, name, .. } => {
                self.class_ref(class, indent);
                self.s("::$");
                self.var_name(name, indent);
            }
            ExprKind::Call { callee, args } => {
                match callee.as_ref() {
                    Callee::Name(n) => self.ns_name(n),
                    Callee::Expr(x) => self.ns_name_or_expr(x, indent),
                }
                self.c(b'(');
                self.args(args, indent);
                self.c(b')');
            }
            ExprKind::MethodCall {
                object,
                name,
                args,
                nullsafe,
            } => {
                self.expr(object, 0, indent);
                self.s(if *nullsafe { "?->" } else { "->" });
                self.member_name(name, indent);
                self.c(b'(');
                self.args(args, indent);
                self.c(b')');
            }
            ExprKind::StaticCall { class, name, args } => {
                match class.as_ref() {
                    ClassRef::Expr(x) if class.is_parent_hook_call(name) => self.expr(x, 0, indent),
                    _ => self.class_ref(class, indent),
                }
                self.s("::");
                self.member_name(name, indent);
                self.c(b'(');
                self.args(args, indent);
                self.c(b')');
            }
            ExprKind::New { class, args } => {
                self.s("new ");
                match class.as_ref() {
                    NewClass::Anonymous(decl) => {
                        self.attributes(&decl.attributes, indent, false);
                        if decl.modifiers.contains(Modifiers::READONLY) {
                            self.s("readonly ");
                        }
                        self.s("class");
                        if !args.args.is_empty() {
                            self.c(b'(');
                            self.args(args, indent);
                            self.c(b')');
                        }
                        self.class_no_header(decl, indent);
                    }
                    NewClass::Name(n) => {
                        self.ns_name(n);
                        self.c(b'(');
                        self.args(args, indent);
                        self.c(b')');
                    }
                    NewClass::Expr(x) => {
                        self.ns_name_or_expr(x, indent);
                        self.c(b'(');
                        self.args(args, indent);
                        self.c(b')');
                    }
                }
            }
            ExprKind::Assign { target, value } => {
                self.binary(" = ", 90, 91, 90, target, value, prio, indent);
            }
            ExprKind::AssignRef { target, value } => {
                self.binary(" =& ", 90, 91, 90, target, value, prio, indent);
            }
            ExprKind::AssignOp { op, target, value } => {
                self.binary(assign_op_str(*op), 90, 91, 90, target, value, prio, indent);
            }
            ExprKind::Binary { op, left, right } => {
                let (s, p, pl, pr) = binary_prec(*op);
                self.binary(s, p, pl, pr, left, right, prio, indent);
            }
            ExprKind::Pipe { left, right } => {
                self.binary(" |> ", 183, 183, 184, left, right, prio, indent);
            }
            ExprKind::Unary { op, expr } => {
                let s = match op {
                    UnaryOp::Plus => "+",
                    UnaryOp::Minus => "-",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "~",
                };
                self.prefix(s, 240, 241, expr, prio, indent);
            }
            ExprKind::IncDec { op, var } => match op {
                IncDecOp::PreInc => self.prefix("++", 240, 241, var, prio, indent),
                IncDecOp::PreDec => self.prefix("--", 240, 241, var, prio, indent),
                IncDecOp::PostInc | IncDecOp::PostDec => {
                    if prio > 240 {
                        self.c(b'(');
                    }
                    self.expr(var, 241, indent);
                    self.s(if *op == IncDecOp::PostInc { "++" } else { "--" });
                    if prio > 240 {
                        self.c(b')');
                    }
                }
            },
            ExprKind::Cast { kind, expr } => {
                let s = match kind {
                    CastKind::Unset => "(unset)",
                    CastKind::Bool => "(bool)",
                    CastKind::Int => "(int)",
                    CastKind::Float => "(float)",
                    CastKind::String => "(string)",
                    CastKind::Array => "(array)",
                    CastKind::Object => "(object)",
                };
                self.prefix(s, 240, 241, expr, prio, indent);
            }
            ExprKind::VoidCast(x) => self.prefix("(void)", 240, 241, x, prio, indent),
            ExprKind::Isset(vars) => {
                // isset($a, $b) is AND(ISSET($a), ISSET($b)) in the Zend AST.
                self.isset_chain(vars, prio, indent);
            }
            ExprKind::Empty(x) => self.func_op("empty", x, indent),
            ExprKind::Silence(x) => self.prefix("@", 240, 241, x, prio, indent),
            ExprKind::Print(x) => self.prefix("print ", 60, 61, x, prio, indent),
            ExprKind::Include { kind, expr } => {
                let s = match kind {
                    IncludeKind::IncludeOnce => "include_once",
                    IncludeKind::Include => "include",
                    IncludeKind::RequireOnce => "require_once",
                    IncludeKind::Require => "require",
                    IncludeKind::Eval => "eval",
                };
                self.func_op(s, expr, indent);
            }
            ExprKind::Instanceof { expr, class } => {
                self.expr(expr, 0, indent);
                self.s(" instanceof ");
                self.class_ref(class, indent);
            }
            ExprKind::Conditional {
                cond, then, else_, ..
            } => {
                if prio > 100 {
                    self.c(b'(');
                }
                self.expr(cond, 100, indent);
                if let Some(t) = then {
                    self.s(" ? ");
                    self.expr(t, 101, indent);
                    self.s(" : ");
                } else {
                    self.s(" ?: ");
                }
                self.expr(else_, 101, indent);
                if prio > 100 {
                    self.c(b')');
                }
            }
            ExprKind::Yield { key, value } => {
                if prio > 70 {
                    self.c(b'(');
                }
                self.s("yield ");
                if let Some(v) = value {
                    if let Some(k) = key {
                        self.expr(k, 70, indent);
                        self.s(" => ");
                    }
                    self.expr(v, 70, indent);
                }
                if prio > 70 {
                    self.c(b')');
                }
            }
            ExprKind::YieldFrom(x) => self.prefix("yield from ", 85, 86, x, prio, indent),
            ExprKind::Throw(x) => {
                self.s("throw ");
                self.expr(x, prio, indent);
            }
            ExprKind::Closure(c) => self.closure(c, indent),
            ExprKind::ArrowFn(f) => self.arrow_fn(f, indent),
            ExprKind::Match { subject, arms } => {
                self.s("match (");
                self.expr(subject, 0, indent);
                self.s(") {\n");
                for arm in arms {
                    self.match_arm(arm, indent + 1);
                }
                self.indent(indent);
                self.c(b'}');
            }
        }
    }

    fn isset_chain(&mut self, vars: &[Expr], prio: i32, indent: usize) {
        match vars {
            [] => {}
            [one] => self.func_op("isset", one, indent),
            [rest @ .., last] => {
                if prio > 130 {
                    self.c(b'(');
                }
                self.isset_chain(rest, 130, indent);
                self.s(" && ");
                // right operand at priority 131
                self.func_op("isset", last, indent);
                if prio > 130 {
                    self.c(b')');
                }
            }
        }
    }

    fn match_arm(&mut self, arm: &MatchArm, indent: usize) {
        self.indent(indent);
        if let Some(conds) = &arm.conds {
            for (i, c) in conds.iter().enumerate() {
                if i > 0 {
                    self.s(", ");
                }
                self.expr(c, 0, indent);
            }
            self.s(" => ");
        } else {
            self.s("default => ");
        }
        self.expr(&arm.body, 0, 0);
        self.s(",\n");
    }

    fn attributes(&mut self, groups: &[AttributeGroup], indent: usize, newlines: bool) {
        for g in groups {
            self.s("#[");
            for (i, a) in g.attributes.iter().enumerate() {
                if i > 0 {
                    self.s(", ");
                }
                self.ns_name(&a.name);
                if let Some(args) = &a.args {
                    self.c(b'(');
                    self.args(args, indent);
                    self.c(b')');
                }
            }
            self.c(b']');
            if newlines {
                self.c(b'\n');
                self.indent(indent);
            } else {
                self.c(b' ');
            }
        }
    }

    /// `zend_ast_export_visibility`
    fn visibility(&mut self, m: Modifiers, with_set: bool) {
        if m.contains(Modifiers::PUBLIC) {
            self.s("public ");
        } else if m.contains(Modifiers::PROTECTED) {
            self.s("protected ");
        } else if m.contains(Modifiers::PRIVATE) {
            self.s("private ");
        }
        if with_set {
            if m.contains(Modifiers::PRIVATE_SET) {
                self.s("private(set) ");
            } else if m.contains(Modifiers::PROTECTED_SET) {
                self.s("protected(set) ");
            } else if m.contains(Modifiers::PUBLIC_SET) {
                self.s("public(set) ");
            }
        }
    }

    fn ty(&mut self, t: &Type) {
        match &t.kind {
            TypeKind::Union(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.c(b'|');
                    }
                    self.ty(t);
                }
            }
            TypeKind::Intersection(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        self.c(b'&');
                    }
                    self.ty(t);
                }
            }
            TypeKind::Nullable(inner) => {
                self.c(b'?');
                self.ty(inner);
            }
            TypeKind::Named(n) => self.ns_name(n),
            TypeKind::Array => self.s("array"),
            TypeKind::Callable => self.s("callable"),
            TypeKind::Static => self.s("static"),
        }
    }

    fn params(&mut self, params: &[Param], indent: usize) {
        for (i, p) in params.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            self.attributes(&p.attributes, indent, false);
            self.visibility(p.modifiers, true);
            if p.modifiers.contains(Modifiers::FINAL) {
                self.s("final ");
            }
            if let Some(t) = &p.ty {
                self.ty(t);
                self.c(b' ');
            }
            if p.by_ref {
                self.c(b'&');
            }
            if p.variadic {
                self.s("...");
            }
            self.c(b'$');
            self.b(&p.name.name);
            if let Some(d) = &p.default {
                self.s(" = ");
                self.expr(d, 0, indent);
            }
            if let Some(h) = &p.hooks {
                self.hooks(h, indent);
            }
        }
    }

    fn hooks(&mut self, hooks: &[PropertyHook], indent: usize) {
        self.s(" {\n");
        let indent = indent + 1;
        self.indent(indent);
        for (i, h) in hooks.iter().enumerate() {
            self.visibility(h.modifiers, true);
            if h.modifiers.contains(Modifiers::FINAL) {
                self.s("final ");
            }
            self.b(&h.name.name);
            match &h.body {
                HookBody::None => self.c(b';'),
                HookBody::Expr(e) => {
                    self.s(" => ");
                    self.expr(e, 0, indent);
                    self.c(b';');
                }
                HookBody::Block(stmts) => {
                    self.s(" {\n");
                    self.stmts(stmts, indent + 1);
                    self.indent(indent);
                    self.c(b'}');
                }
            }
            if i + 1 < hooks.len() {
                self.c(b'\n');
                self.indent(indent);
            }
        }
        self.c(b'\n');
        self.indent(indent - 1);
        self.c(b'}');
    }

    fn closure(&mut self, c: &Closure, indent: usize) {
        self.attributes(&c.attributes, indent, false);
        if c.is_static {
            self.s("static ");
        }
        self.s("function ");
        if c.by_ref {
            self.c(b'&');
        }
        self.c(b'(');
        self.params(&c.params, indent);
        self.c(b')');
        if !c.uses.is_empty() {
            self.s(" use(");
            for (i, u) in c.uses.iter().enumerate() {
                if i > 0 {
                    self.s(", ");
                }
                if u.by_ref {
                    self.c(b'&');
                }
                self.c(b'$');
                self.b(&u.name.name);
            }
            self.c(b')');
        }
        if let Some(t) = &c.return_type {
            self.s(": ");
            self.ty(t);
        }
        self.s(" {\n");
        self.stmts(&c.body, indent + 1);
        self.indent(indent);
        self.c(b'}');
    }

    fn arrow_fn(&mut self, f: &ArrowFn, indent: usize) {
        if f.parenthesized {
            self.c(b'(');
        }
        self.attributes(&f.attributes, indent, false);
        if f.is_static {
            self.s("static ");
        }
        self.s("fn");
        if f.by_ref {
            self.c(b'&');
        }
        self.c(b'(');
        self.params(&f.params, indent);
        self.c(b')');
        if let Some(t) = &f.return_type {
            self.s(": ");
            self.ty(t);
        }
        self.s(" => ");
        self.expr(&f.body, 0, indent);
        if f.parenthesized {
            self.c(b')');
        }
    }

    #[allow(clippy::too_many_arguments)] // mirrors the fields of `zend_ast_decl`
    fn function_like(
        &mut self,
        attributes: &[AttributeGroup],
        modifiers: Modifiers,
        by_ref: bool,
        name: &[u8],
        params: &[Param],
        return_type: Option<&Type>,
        body: Option<&[Stmt]>,
        indent: usize,
    ) {
        self.attributes(attributes, indent, true);
        self.visibility(modifiers, false);
        if modifiers.contains(Modifiers::STATIC) {
            self.s("static ");
        }
        if modifiers.contains(Modifiers::ABSTRACT) {
            self.s("abstract ");
        }
        if modifiers.contains(Modifiers::FINAL) {
            self.s("final ");
        }
        self.s("function ");
        if by_ref {
            self.c(b'&');
        }
        self.b(name);
        self.c(b'(');
        self.params(params, indent);
        self.c(b')');
        if let Some(t) = return_type {
            self.s(": ");
            self.ty(t);
        }
        if let Some(body) = body {
            self.s(" {\n");
            self.stmts(body, indent + 1);
            self.indent(indent);
            self.s("}\n");
        } else {
            self.s(";\n");
        }
    }

    fn class_no_header(&mut self, decl: &ClassDecl, indent: usize) {
        if let Some(p) = &decl.extends {
            self.s(" extends ");
            self.ns_name(p);
        }
        if !decl.implements.is_empty() {
            self.s(" implements ");
            self.name_list(&decl.implements, ", ");
        }
        self.s(" {\n");
        for m in &decl.members {
            self.member(m, indent + 1);
        }
        self.indent(indent);
        self.c(b'}');
    }

    fn name_list(&mut self, names: &[Name], sep: &str) {
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                self.s(sep);
            }
            // `zend_ast_export_name` prints the stored string (no prefix).
            self.b(&n.name);
        }
    }

    fn class_decl(&mut self, decl: &ClassDecl, indent: usize) {
        self.attributes(&decl.attributes, indent, true);
        match decl.kind {
            ClassKind::Interface => self.s("interface "),
            ClassKind::Trait => self.s("trait "),
            ClassKind::Enum => self.s("enum "),
            ClassKind::Class => {
                if decl.modifiers.contains(Modifiers::ABSTRACT) {
                    self.s("abstract ");
                }
                if decl.modifiers.contains(Modifiers::FINAL) {
                    self.s("final ");
                }
                if decl.modifiers.contains(Modifiers::READONLY) {
                    self.s("readonly ");
                }
                self.s("class ");
            }
        }
        if let Some(n) = &decl.name {
            self.b(&n.name);
        }
        if decl.kind == ClassKind::Enum {
            if let Some(t) = &decl.backing_type {
                self.s(": ");
                self.ty(t);
            }
        }
        self.class_no_header(decl, indent);
        self.c(b'\n');
    }

    fn const_elems(&mut self, consts: &[ConstElem], indent: usize) {
        for (i, c) in consts.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            self.b(&c.name.name);
            self.s(" = ");
            self.expr(&c.value, 0, indent);
        }
    }

    /// A class member as a statement of the class body.
    fn member(&mut self, m: &ClassMember, indent: usize) {
        self.indent(indent);
        match &m.kind {
            ClassMemberKind::Property(g) => {
                self.prop_group(g, indent);
                if g.props.first().is_none_or(|p| p.hooks.is_none()) {
                    self.c(b';');
                }
            }
            ClassMemberKind::Const(g) => {
                self.const_group(g, indent);
                self.c(b';');
            }
            ClassMemberKind::Method(md) => self.method(md, indent),
            ClassMemberKind::TraitUse(tu) => {
                self.s("use ");
                self.name_list(&tu.traits, ", ");
                if tu.adaptations.is_empty() {
                    self.c(b';');
                } else {
                    self.s(" {\n");
                    for a in &tu.adaptations {
                        self.indent(indent + 1);
                        self.adaptation(a);
                        self.s(";\n");
                    }
                    self.indent(indent);
                    self.c(b'}');
                }
            }
            ClassMemberKind::EnumCase(c) => {
                self.attributes(&c.attributes, indent, true);
                self.s("case ");
                self.b(&c.name.name);
                if let Some(v) = &c.value {
                    self.s(" = ");
                    self.expr(v, 0, indent);
                }
                self.c(b';');
            }
        }
        self.c(b'\n');
    }

    fn prop_group(&mut self, g: &PropertyGroup, indent: usize) {
        self.attributes(&g.attributes, indent, true);
        self.visibility(g.modifiers, true);
        if g.modifiers.contains(Modifiers::STATIC) {
            self.s("static ");
        }
        if g.modifiers.contains(Modifiers::ABSTRACT) {
            self.s("abstract ");
        }
        if g.modifiers.contains(Modifiers::FINAL) {
            self.s("final ");
        }
        if g.modifiers.contains(Modifiers::READONLY) {
            self.s("readonly ");
        }
        if let Some(t) = &g.ty {
            self.ty(t);
            self.c(b' ');
        }
        for (i, p) in g.props.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            self.c(b'$');
            self.b(&p.name.name);
            if let Some(d) = &p.default {
                self.s(" = ");
                self.expr(d, 0, indent + 1);
            }
            if let Some(h) = &p.hooks {
                self.hooks(h, indent);
            }
        }
    }

    fn const_group(&mut self, g: &ClassConstGroup, indent: usize) {
        self.attributes(&g.attributes, indent, true);
        let mut m = g.modifiers;
        if !m.intersects(Modifiers::VISIBILITY) {
            m |= Modifiers::PUBLIC;
        }
        self.visibility(m, false);
        if m.contains(Modifiers::FINAL) {
            self.s("final ");
        }
        self.s("const ");
        if let Some(t) = &g.ty {
            self.ty(t);
            self.c(b' ');
        }
        self.const_elems(&g.consts, indent);
    }

    fn method(&mut self, md: &MethodDecl, indent: usize) {
        let mut m = md.modifiers;
        if !m.intersects(Modifiers::VISIBILITY) {
            m |= Modifiers::PUBLIC;
        }
        self.function_like(
            &md.attributes,
            m,
            md.by_ref,
            &md.name.name,
            &md.params,
            md.return_type.as_ref(),
            md.body.as_deref(),
            indent,
        );
    }

    fn method_ref(&mut self, r: &TraitMethodRef) {
        if let Some(t) = &r.trait_name {
            self.b(&t.name);
            self.s("::");
        }
        self.b(&r.method.name);
    }

    fn adaptation(&mut self, a: &TraitAdaptation) {
        match a {
            TraitAdaptation::Precedence {
                method, insteadof, ..
            } => {
                self.method_ref(method);
                self.s(" insteadof ");
                self.name_list(insteadof, ", ");
            }
            TraitAdaptation::Alias {
                method,
                modifiers,
                alias,
                ..
            } => {
                self.method_ref(method);
                if modifiers.contains(Modifiers::PUBLIC) {
                    self.s(" as public");
                } else if modifiers.contains(Modifiers::PROTECTED) {
                    self.s(" as protected");
                } else if modifiers.contains(Modifiers::PRIVATE) {
                    self.s(" as private");
                } else if alias.is_some() {
                    self.s(" as");
                }
                if let Some(al) = alias {
                    self.c(b' ');
                    self.b(&al.name);
                }
            }
        }
    }

    fn stmts(&mut self, stmts: &[Stmt], indent: usize) {
        for s in stmts {
            self.stmt(s, indent);
        }
    }

    /// The body of a control structure (`zend_ast_export_stmt` on a
    /// statement that may be a list).
    fn body(&mut self, s: &Stmt, indent: usize) {
        self.stmt(s, indent);
    }

    /// `zend_ast_export_stmt`
    #[allow(clippy::too_many_lines)]
    fn stmt(&mut self, s: &Stmt, indent: usize) {
        match &s.kind {
            StmtKind::Block(list) => return self.stmts(list, indent),
            StmtKind::Nop => return,
            StmtKind::Echo(exprs) => {
                for e in exprs {
                    self.simple_stmt(indent, |x| {
                        x.s("echo ");
                        x.expr(e, 0, indent);
                    });
                }
                return;
            }
            StmtKind::InlineHtml(html) => {
                self.simple_stmt(indent, |x| {
                    x.s("echo ");
                    x.quoted_str(html);
                });
                return;
            }
            StmtKind::Global(vars) => {
                for v in vars {
                    self.simple_stmt(indent, |x| {
                        x.s("global ");
                        x.expr(v, 0, indent);
                    });
                }
                return;
            }
            StmtKind::Static(vars) => {
                for v in vars {
                    self.simple_stmt(indent, |x| {
                        x.s("static $");
                        x.b(&v.name.name);
                        if let Some(d) = &v.default {
                            x.s(" = ");
                            x.expr(d, 0, indent);
                        }
                    });
                }
                return;
            }
            StmtKind::Unset(vars) => {
                for v in vars {
                    self.simple_stmt(indent, |x| x.func_op("unset", v, indent));
                }
                return;
            }
            _ => {}
        }
        self.indent(indent);
        let terminated = match &s.kind {
            StmtKind::Expr(e) => {
                self.expr(e, 0, indent);
                false
            }
            StmtKind::Return(e) | StmtKind::Break(e) | StmtKind::Continue(e) => {
                self.s(match &s.kind {
                    StmtKind::Return(_) => "return",
                    StmtKind::Break(_) => "break",
                    _ => "continue",
                });
                if let Some(e) = e {
                    self.c(b' ');
                    self.expr(e, 0, indent);
                }
                false
            }
            StmtKind::Const { consts, attributes } => {
                self.attributes(attributes, indent, true);
                self.s("const ");
                self.const_elems(consts, indent);
                false
            }
            StmtKind::If {
                branches,
                else_branch,
            } => {
                self.if_stmt(branches, else_branch.as_deref(), indent);
                true
            }
            StmtKind::While { cond, body } => {
                self.s("while (");
                self.expr(cond, 0, indent);
                self.s(") {\n");
                self.body(body, indent + 1);
                self.indent(indent);
                self.c(b'}');
                true
            }
            StmtKind::DoWhile { body, cond } => {
                self.s("do {\n");
                self.body(body, indent + 1);
                self.indent(indent);
                self.s("} while (");
                self.expr(cond, 0, indent);
                self.c(b')');
                false
            }
            StmtKind::For {
                init,
                cond,
                step,
                body,
            } => {
                self.s("for (");
                self.expr_list(init, indent);
                self.c(b';');
                if !cond.is_empty() {
                    self.c(b' ');
                    self.expr_list(cond, indent);
                }
                self.c(b';');
                if !step.is_empty() {
                    self.c(b' ');
                    self.expr_list(step, indent);
                }
                self.s(") {\n");
                self.body(body, indent + 1);
                self.indent(indent);
                self.c(b'}');
                true
            }
            StmtKind::Foreach {
                subject,
                key,
                value,
                by_ref,
                body,
            } => {
                self.s("foreach (");
                self.expr(subject, 0, indent);
                self.s(" as ");
                if let Some(k) = key {
                    self.expr(k, 0, indent);
                    self.s(" => ");
                }
                if *by_ref {
                    self.c(b'&');
                }
                self.expr(value, 0, indent);
                self.s(") {\n");
                self.body(body, indent + 1);
                self.indent(indent);
                self.c(b'}');
                true
            }
            StmtKind::Switch { subject, cases } => {
                self.s("switch (");
                self.expr(subject, 0, indent);
                self.s(") {\n");
                for c in cases {
                    self.indent(indent + 1);
                    if let Some(v) = &c.cond {
                        self.s("case ");
                        self.expr(v, 0, indent + 1);
                        self.s(":\n");
                    } else {
                        self.s("default:\n");
                    }
                    self.stmts(&c.body, indent + 2);
                }
                self.indent(indent);
                self.c(b'}');
                true
            }
            StmtKind::Declare { directives, body } => {
                self.s("declare(");
                self.const_elems(directives, indent);
                self.c(b')');
                if let Some(b) = body {
                    self.s(" {\n");
                    self.body(b, indent + 1);
                    self.indent(indent);
                    self.c(b'}');
                } else {
                    self.c(b';');
                }
                true
            }
            StmtKind::Try {
                body,
                catches,
                finally,
            } => {
                self.s("try {\n");
                self.stmts(body, indent + 1);
                self.indent(indent);
                for c in catches {
                    self.s("} catch (");
                    self.name_list(&c.types, "|");
                    if let Some(v) = &c.var {
                        self.s(" $");
                        self.var_ident(&v.name);
                    }
                    self.s(") {\n");
                    self.stmts(&c.body, indent + 1);
                    self.indent(indent);
                }
                if let Some(f) = finally {
                    self.s("} finally {\n");
                    self.stmts(f, indent + 1);
                    self.indent(indent);
                }
                self.c(b'}');
                true
            }
            StmtKind::Goto(l) => {
                self.s("goto ");
                self.b(&l.name);
                false
            }
            StmtKind::Label(l) => {
                self.b(&l.name);
                self.c(b':');
                true
            }
            StmtKind::Function(f) => {
                self.function_like(
                    &f.attributes,
                    Modifiers::NONE,
                    f.by_ref,
                    &f.name.name,
                    &f.params,
                    f.return_type.as_ref(),
                    Some(&f.body),
                    indent,
                );
                true
            }
            StmtKind::Class(c) => {
                self.class_decl(c, indent);
                true
            }
            StmtKind::HaltCompiler { .. } => {
                self.s("__HALT_COMPILER()");
                false
            }
            StmtKind::Namespace { name, body } => {
                self.s("namespace");
                if let Some(n) = name {
                    self.c(b' ');
                    self.b(&n.name);
                }
                if let Some(b) = body {
                    self.s(" {\n");
                    self.stmts(b, indent + 1);
                    self.indent(indent);
                    self.s("}\n");
                } else {
                    self.c(b';');
                }
                true
            }
            StmtKind::Use { kind, uses } => {
                self.s("use ");
                self.use_kind(*kind);
                self.use_items(uses, false);
                false
            }
            StmtKind::GroupUse { kind, prefix, uses } => {
                self.s("use ");
                if let Some(k) = kind {
                    self.use_kind(*k);
                }
                self.b(&prefix.name);
                self.s("\\{");
                self.use_items(uses, kind.is_none());
                self.c(b'}');
                false
            }
            StmtKind::Block(_)
            | StmtKind::Nop
            | StmtKind::Echo(_)
            | StmtKind::InlineHtml(_)
            | StmtKind::Global(_)
            | StmtKind::Static(_)
            | StmtKind::Unset(_) => true,
        };
        if !terminated {
            self.c(b';');
        }
        self.c(b'\n');
    }

    fn simple_stmt(&mut self, indent: usize, f: impl FnOnce(&mut Self)) {
        self.indent(indent);
        f(self);
        self.s(";\n");
    }

    fn use_kind(&mut self, k: UseKind) {
        match k {
            UseKind::Function => self.s("function "),
            UseKind::Const => self.s("const "),
            UseKind::Class => {}
        }
    }

    fn use_items(&mut self, uses: &[UseItem], per_item_kind: bool) {
        for (i, u) in uses.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            if per_item_kind {
                self.use_kind(u.kind);
            }
            self.b(&u.name.name);
            if let Some(a) = &u.alias {
                self.s(" as ");
                self.b(&a.name);
            }
        }
    }

    fn expr_list(&mut self, list: &[Expr], indent: usize) {
        for (i, e) in list.iter().enumerate() {
            if i > 0 {
                self.s(", ");
            }
            self.expr(e, 20, indent);
        }
    }

    fn if_stmt(
        &mut self,
        branches: &[crate::ast::IfBranch],
        else_branch: Option<&Stmt>,
        indent: usize,
    ) {
        for (i, b) in branches.iter().enumerate() {
            if i == 0 {
                self.s("if (");
            } else {
                self.indent(indent);
                self.s("} elseif (");
            }
            self.expr(&b.cond, 0, indent);
            self.s(") {\n");
            self.body(&b.body, indent + 1);
        }
        if let Some(e) = else_branch {
            self.indent(indent);
            self.s("} else ");
            if let StmtKind::If {
                branches,
                else_branch,
            } = &e.kind
            {
                return self.if_stmt(branches, else_branch.as_deref(), indent);
            }
            self.s("{\n");
            self.body(e, indent + 1);
        }
        self.indent(indent);
        self.c(b'}');
    }
}

/// Whether an expression is a `ZEND_AST_ZVAL` literal in the Zend AST.
fn is_zval(e: &Expr) -> bool {
    matches!(
        e.kind,
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::String(_)
    )
}

#[cfg(test)]
mod tests {
    use super::format_double;

    #[test]
    fn doubles() {
        assert_eq!(format_double(1.5, 14), "1.5");
        assert_eq!(format_double(1.0, 14), "1.0");
        assert_eq!(format_double(0.0, 14), "0.0");
        assert_eq!(format_double(-0.0, 14), "-0.0");
        assert_eq!(format_double(1e25, 14), "1.0E+25");
        assert_eq!(format_double(0.0001, 14), "0.0001");
        assert_eq!(format_double(0.00001, 14), "1.0E-5");
        assert_eq!(format_double(0.1 + 0.2, 14), "0.3");
        assert_eq!(
            format_double(123_456_789_012_345.0, 14),
            "1.2345678901234E+14"
        );
        assert_eq!(format_double(100.0, 14), "100.0");
        assert_eq!(format_double(f64::INFINITY, 14), "INF");
        assert_eq!(format_double(1.5e-7, 14), "1.5E-7");
    }
}
