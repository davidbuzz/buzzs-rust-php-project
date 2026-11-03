//! Expression evaluation, assignments and calls.

use super::interp::*;
use php_parser::ast::*;
use php_runtime::{
    ops, ArrayKey, CastTarget, ErrorLevel, HashTable, Obj, PhpRef, PropertyPurpose, RtResult,
    RuntimeContext, RuntimeError, Value, ZArray, ZStr,
};
use php_spl::DimKind;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

fn s(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// A write target: a base and a path of dimension keys (`None` = `[]`).
pub enum LBase {
    Var(Vec<u8>),
    Prop(Obj, String),
    StaticProp(Rc<Class>, String),
    Temp(Value),
}

pub enum Op {
    Assign(Value),
    AssignOp(php_parser::ast::BinaryOp, Value),
    MakeRef,
    SetRef(PhpRef),
    Unset,
    IncDec(IncDecOp),
}

pub fn runtime_binop(op: php_parser::ast::BinaryOp) -> Option<php_runtime::BinaryOp> {
    use php_parser::ast::BinaryOp as P;
    use php_runtime::BinaryOp as R;
    Some(match op {
        P::Add => R::Add,
        P::Sub => R::Sub,
        P::Mul => R::Mul,
        P::Div => R::Div,
        P::Mod => R::Mod,
        P::Pow => R::Pow,
        P::Concat => R::Concat,
        P::ShiftLeft => R::ShiftLeft,
        P::ShiftRight => R::ShiftRight,
        P::BitOr => R::BitOr,
        P::BitAnd => R::BitAnd,
        P::BitXor => R::BitXor,
        P::BoolXor => R::BoolXor,
        _ => return None,
    })
}

pub fn to_key(it: &mut Interp, k: &Value) -> RtResult<ArrayKey> {
    Ok(match k.deref() {
        Value::Int(n) => ArrayKey::Int(n),
        Value::Str(s) => ArrayKey::from_zstr(&s),
        Value::Bool(b) => ArrayKey::Int(i64::from(b)),
        Value::Float(f) => ArrayKey::Int(php_runtime::convert::dval_to_lval_safe(f, it)?),
        Value::Null => ArrayKey::Str(ZStr::empty()),
        Value::Resource(r) => {
            it.emit(
                ErrorLevel::Warning,
                &format!("Resource ID#{} used as offset, casting to integer ({})", r.handle(), r.handle()),
            )?;
            ArrayKey::Int(r.handle())
        }
        other => {
            return Err(RuntimeError::TypeError(format!(
                "Cannot access offset of type {} on array",
                other.type_name()
            )))
        }
    })
}

fn key_display(k: &ArrayKey) -> String {
    match k {
        ArrayKey::Int(i) => i.to_string(),
        ArrayKey::Str(s) => format!("\"{}\"", s.to_string_lossy()),
    }
}

impl Interp {
    pub fn eval(&mut self, e: &Expr) -> RtResult<Value> {
        if e.span.line > 0 {
            self.line = e.span.line;
        }
        match &e.kind {
            ExprKind::Int(i) => Ok(Value::Int(*i)),
            ExprKind::Float(f) => Ok(Value::Float(*f)),
            ExprKind::String(b) => Ok(Value::Str(ZStr::from(b.as_slice()))),
            ExprKind::Interpolated(parts) => {
                let mut out = Vec::new();
                for p in parts {
                    match p {
                        InterpPart::Literal(b, _) => out.extend_from_slice(b),
                        InterpPart::Expr { expr, .. } => {
                            let v = self.eval(expr)?;
                            out.extend_from_slice(v.to_zstr(self)?.as_bytes());
                        }
                    }
                }
                Ok(Value::Str(ZStr::from(out)))
            }
            ExprKind::ShellExec(_) => Err(rt_err("shell exec unsupported")),
            ExprKind::Array { items, .. } => self.eval_array(items),
            ExprKind::Const(n) => self.eval_const(n),
            ExprKind::MagicConst(m) => Ok(match m {
                MagicConst::Line => Value::Int(i64::from(e.span.line)),
                MagicConst::File => Value::from(self.file.as_str()),
                MagicConst::Dir => Value::from(self.dir.as_str()),
                MagicConst::Class => Value::from(self.frame().scope.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
                MagicConst::Function => {
                    let f = self.frame().func.clone();
                    Value::from(f.rsplit("::").next().unwrap_or("").to_string())
                }
                MagicConst::Method => Value::from(self.frame().func.clone()),
                _ => Value::from(""),
            }),
            ExprKind::ClassConst { class, name } => {
                let n = self.member_name(name)?;
                if n.eq_ignore_ascii_case("class") {
                    if let ClassRef::Name(nm) = class.as_ref() {
                        let l = lc(&nm.name);
                        if l != "self" && l != "static" && l != "parent" {
                            let raw = nm.name.strip_prefix(b"\\").unwrap_or(&nm.name);
                            return Ok(Value::Str(ZStr::from(raw)));
                        }
                    }
                    let c = self.resolve_class_ref(class)?;
                    return Ok(Value::from(c.name.clone()));
                }
                let c = self.resolve_class_ref(class)?;
                self.class_const(&c, &n)
            }
            ExprKind::ClassName(c) => {
                let c = self.resolve_class_ref(c)?;
                Ok(Value::from(c.name.clone()))
            }
            ExprKind::Var(v) => {
                let n = self.var_name(v)?;
                self.get_var(&n)
            }
            ExprKind::Dim { base, index } => {
                let b = self.eval(base)?;
                let Some(index) = index else {
                    return Err(rt_err("Cannot use [] for reading"));
                };
                let k = self.eval(index)?;
                self.read_dim(&b, &k, DimKind::Read)
            }
            ExprKind::Prop { object, name, nullsafe } => {
                let o = self.eval(object)?;
                if *nullsafe && o.is_null() {
                    return Ok(Value::Null);
                }
                let n = self.member_name(name)?;
                self.read_prop(&o, &n)
            }
            ExprKind::StaticProp { class, name, .. } => {
                let c = self.resolve_class_ref(class)?;
                let n = s(&self.var_name(name)?);
                self.static_prop(&c, &n).map(|r| r.get())
            }
            ExprKind::Call { callee, args } => self.eval_call(callee, args),
            ExprKind::MethodCall { object, name, args, nullsafe } => {
                let o = self.eval(object)?;
                if *nullsafe && o.is_null() {
                    return Ok(Value::Null);
                }
                let n = self.member_name(name)?;
                self.eval_method_call(o, &n, args)
            }
            ExprKind::StaticCall { class, name, args } => {
                let n = self.member_name(name)?;
                self.eval_static_call(class, &n, args)
            }
            ExprKind::New { class, args } => {
                let c = match class.as_ref() {
                    NewClass::Name(n) => self.resolve_class_name(&n.name)?,
                    NewClass::Expr(e) => {
                        let v = self.eval(e)?;
                        match v {
                            Value::Object(o) => tobj(&o).class.clone(),
                            other => {
                                let n = other.to_zstr(self)?;
                                self.lookup_class(n.as_bytes())?
                            }
                        }
                    }
                    NewClass::Anonymous(decl) => self.declare_class(decl)?,
                };
                let by_ref = self.ctor_by_ref(&c);
                let o = self.new_object_raw(&c)?;
                let argv = self.eval_args(args, &|i| by_ref.get(i).copied().unwrap_or(false))?;
                if let Some(m) = c.find_method("__construct") {
                    self.call_method_h(Some(&o), &c, &m, &argv)?;
                }
                Ok(Value::Object(o))
            }
            ExprKind::Assign { target, value } => {
                if let ExprKind::Array { items, .. } = &target.kind {
                    let v = self.eval(value)?;
                    self.destructure(items, &v)?;
                    return Ok(v);
                }
                let (base, path) = self.lvalue(target)?;
                let v = self.eval(value)?;
                let v = match v {
                    Value::Reference(r) => r.get(),
                    Value::Array(a) => Value::Array(a),
                    other => other,
                };
                self.apply(base, &path, Op::Assign(v))
            }
            ExprKind::AssignRef { target, value } => {
                let r = match &value.kind {
                    ExprKind::New { .. } | ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::StaticCall { .. } => {
                        let v = self.eval(value)?;
                        match v {
                            Value::Reference(r) => r,
                            other => PhpRef::new(other),
                        }
                    }
                    _ => self.get_ref(value)?,
                };
                self.assign_ref_to(target, r.clone())?;
                Ok(r.get())
            }
            ExprKind::AssignOp { op, target, value } => {
                if *op == php_parser::ast::BinaryOp::Coalesce {
                    let cur = self.eval_quiet(target)?;
                    if let Some(v) = cur {
                        if !v.is_null() {
                            return Ok(v);
                        }
                    }
                    let v = self.eval(value)?;
                    return self.assign(target, v);
                }
                let (base, path) = self.lvalue(target)?;
                let v = self.eval(value)?;
                self.apply(base, &path, Op::AssignOp(*op, v.deref()))
            }
            ExprKind::Binary { op, left, right } => self.eval_binary(*op, left, right),
            ExprKind::Unary { op, expr } => {
                let v = self.eval(expr)?;
                match op {
                    UnaryOp::Not => Ok(Value::Bool(!v.to_bool(self)?)),
                    UnaryOp::Minus => ops::mul(&v, &Value::Int(-1), self),
                    UnaryOp::Plus => ops::mul(&v, &Value::Int(1), self),
                    UnaryOp::BitNot => ops::bit_not(&v, self),
                }
            }
            ExprKind::IncDec { op, var } => {
                let (base, path) = self.lvalue(var)?;
                self.apply(base, &path, Op::IncDec(*op))
            }
            ExprKind::Cast { kind, expr } => {
                let v = self.eval(expr)?;
                self.cast(*kind, v)
            }
            ExprKind::VoidCast(e) => {
                self.eval(e)?;
                Ok(Value::Null)
            }
            ExprKind::Isset(es) => {
                for e in es {
                    if !self.eval_isset(e)? {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            ExprKind::Empty(e) => Ok(Value::Bool(self.eval_empty(e)?)),
            ExprKind::Silence(e) => {
                let old = self.error_reporting;
                self.error_reporting = 0;
                let r = self.eval(e);
                self.error_reporting = old;
                r
            }
            ExprKind::Print(e) => {
                let v = self.eval(e)?;
                let z = v.to_zstr(self)?;
                self.echo(z.as_bytes());
                Ok(Value::Int(1))
            }
            ExprKind::Include { kind, expr } => {
                let v = self.eval(expr)?;
                let p = v.to_zstr(self)?.to_string_lossy();
                self.include_file(&p, *kind)
            }
            ExprKind::Instanceof { expr, class } => {
                let v = self.eval(expr)?;
                let Value::Object(o) = v else {
                    return Ok(Value::Bool(false));
                };
                let name = match class.as_ref() {
                    ClassRef::Name(n) => {
                        let l = lc(&n.name);
                        if l == "self" || l == "static" || l == "parent" {
                            self.resolve_class_name(&n.name)?.name.clone()
                        } else {
                            s(n.name.strip_prefix(b"\\").unwrap_or(&n.name))
                        }
                    }
                    ClassRef::Expr(e) => match self.eval(e)? {
                        Value::Object(c) => tobj(&c).class.name.clone(),
                        other => other.to_zstr(self)?.to_string_lossy(),
                    },
                };
                Ok(Value::Bool(tobj(&o).class.is_a(&name)))
            }
            ExprKind::Conditional { cond, then, else_, .. } => {
                let c = self.eval(cond)?;
                if c.to_bool(self)? {
                    match then {
                        Some(t) => self.eval(t),
                        None => Ok(c),
                    }
                } else {
                    self.eval(else_)
                }
            }
            ExprKind::Throw(e) => {
                let v = self.eval(e)?;
                match v {
                    Value::Object(o) if tobj(&o).class.is_a("Throwable") => Err(RuntimeError::Thrown(Value::Object(o))),
                    _ => Err(rt_err("Can only throw objects")),
                }
            }
            ExprKind::Closure(c) => {
                let mut bound = Vec::new();
                for u in &c.uses {
                    let v = if u.by_ref {
                        let r = {
                            let vars = &mut self.frame().vars;
                            vars.entry(u.name.name.clone()).or_insert(Value::Null).make_ref()
                        };
                        Value::Reference(r)
                    } else {
                        match self.frame().vars.get(&u.name.name) {
                            Some(v) => v.deref(),
                            None => {
                                self.emit(ErrorLevel::Warning, &format!("Undefined variable ${}", s(&u.name.name)))?;
                                Value::Null
                            }
                        }
                    };
                    bound.push((u.name.name.clone(), v));
                }
                let def = FuncDef { return_type: None, name: "{closure}".into(),
                    params: c.params.clone(),
                    body: Rc::new(c.body.clone()),
                    arrow: None,
                    by_ref: c.by_ref,
                    is_generator: c.is_generator,
                    statics: Rc::new(RefCell::new(HashMap::new())),
                };
                let this = if c.is_static { None } else { self.frame().this.clone() };
                let scope = self.frame().scope.clone();
                self.make_closure(ClosureData { func: Rc::new(def), bound, this, scope, builtin: None, method: None })
            }
            ExprKind::ArrowFn(a) => {
                let bound: Vec<(Vec<u8>, Value)> =
                    self.frame().vars.iter().map(|(k, v)| (k.clone(), v.deref())).collect();
                let def = FuncDef { return_type: None, name: "{closure}".into(),
                    params: a.params.clone(),
                    body: Rc::new(Vec::new()),
                    arrow: Some(Rc::new((*a.body).clone())),
                    by_ref: a.by_ref,
                    is_generator: false,
                    statics: Rc::new(RefCell::new(HashMap::new())),
                };
                let this = if a.is_static { None } else { self.frame().this.clone() };
                let scope = self.frame().scope.clone();
                self.make_closure(ClosureData { func: Rc::new(def), bound, this, scope, builtin: None, method: None })
            }
            ExprKind::Match { subject, arms } => {
                let v = self.eval(subject)?;
                for arm in arms {
                    match &arm.conds {
                        None => return self.eval(&arm.body),
                        Some(cs) => {
                            for c in cs {
                                let cv = self.eval(c)?;
                                if ops::is_identical(&v, &cv)? {
                                    return self.eval(&arm.body);
                                }
                            }
                        }
                    }
                }
                let desc = match &v {
                    Value::Str(st) => format!("'{}'", st.to_string_lossy()),
                    Value::Object(_) => "of type object".into(),
                    other => other.to_zstr(self)?.to_string_lossy(),
                };
                Err(RuntimeError::Custom { class: ZStr::from("UnhandledMatchError"), message: format!("Unhandled match case {desc}") })
            }
            ExprKind::Yield { .. } | ExprKind::YieldFrom(_) => Err(rt_err("generators unsupported")),
            ExprKind::Pipe { left, right } => {
                let l = self.eval(left)?;
                let f = self.eval(right)?;
                self.call_value(&f, &[l])
            }
        }
    }

    pub fn make_closure(&mut self, d: ClosureData) -> RtResult<Value> {
        let cls = self.find_class(b"Closure").expect("Closure class");
        let o = self.new_object_raw(&cls)?;
        *tobj(&o).extra.borrow_mut() = Extra::Closure(Rc::new(d));
        Ok(Value::Object(o))
    }

    fn eval_array(&mut self, items: &[Option<ArrayItem>]) -> RtResult<Value> {
        let mut t = HashTable::new();
        for it in items.iter().flatten() {
            if it.unpack {
                let v = self.eval(&it.value)?;
                match v {
                    Value::Array(a) => {
                        for (k, v) in a.iter() {
                            match k {
                                ArrayKey::Int(_) => {
                                    let _ = t.push(v.deref());
                                }
                                ArrayKey::Str(_) => {
                                    t.insert(k.clone(), v.deref());
                                }
                            }
                        }
                    }
                    Value::Object(o) => {
                        let mut iter = php_spl::iter::get_iterator(self, &o, false)?;
                        iter.rewind(self)?;
                        while iter.valid(self)? {
                            let Some(v) = iter.current(self)? else { break };
                            let k = iter.key(self)?.unwrap_or_default();
                            match k {
                                Value::Str(sk) => {
                                    t.insert(ArrayKey::from_zstr(&sk), v);
                                }
                                _ => {
                                    let _ = t.push(v);
                                }
                            }
                            iter.next(self)?;
                        }
                    }
                    _ => return Err(rt_err("Only arrays and Traversables can be unpacked")),
                }
                continue;
            }
            let v = if it.by_ref { Value::Reference(self.get_ref(&it.value)?) } else { self.eval(&it.value)?.deref() };
            match &it.key {
                Some(k) => {
                    let kv = self.eval(k)?;
                    let key = to_key(self, &kv)?;
                    t.insert(key, v);
                }
                None => {
                    if t.push(v).is_err() {
                        return Err(rt_err("Cannot add element to the array as the next element is already occupied"));
                    }
                }
            }
        }
        Ok(Value::Array(ZArray::from_table(t)))
    }

    fn eval_const(&mut self, n: &Name) -> RtResult<Value> {
        let raw = n.name.strip_prefix(b"\\").unwrap_or(&n.name);
        let name = s(raw);
        let l = name.to_ascii_lowercase();
        match l.as_str() {
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            "null" => return Ok(Value::Null),
            _ => {}
        }
        if let Some(v) = self.constants.get(&name) {
            return Ok(v.clone());
        }
        Err(rt_err(format!("Undefined constant \"{name}\"")))
    }

    pub fn class_const(&mut self, c: &Rc<Class>, n: &str) -> RtResult<Value> {
        if c.internal {
            if let Some(v) = php_spl::classes::constant(&c.name, n) {
                return Ok(Value::Int(v));
            }
        }
        match c.find_const(n) {
            Some((_, Ok(v))) => Ok(v),
            Some((owner, Err(e))) => {
                self.frames.push(Frame {
                    scope: Some(owner.clone()),
                    static_scope: Some(owner.clone()),
                    vars: HashMap::new(),
                    this: None,
                    func: String::new(),
                    args: Vec::new(),
                    func_def: None,
                });
                let v = self.eval(&e);
                self.frames.pop();
                let v = v?;
                if let Some(slot) = owner.consts.borrow_mut().iter_mut().find(|(k, _)| k == n) {
                    slot.1 = Ok(v.clone());
                }
                Ok(v)
            }
            None => {
                let mut k = c.parent.clone();
                while let Some(p) = k {
                    if p.internal {
                        if let Some(v) = php_spl::classes::constant(&p.name, n) {
                            return Ok(Value::Int(v));
                        }
                    }
                    k = p.parent.clone();
                }
                Err(rt_err(format!("Undefined constant {}::{n}", c.name)))
            }
        }
    }

    pub fn static_prop(&mut self, c: &Rc<Class>, n: &str) -> RtResult<PhpRef> {
        let mut k = Some(c.clone());
        while let Some(cl) = k {
            let mut st = cl.statics.borrow_mut();
            if let Some(v) = st.get_mut(n) {
                return Ok(v.make_ref());
            }
            drop(st);
            k = cl.parent.clone();
        }
        Err(rt_err(format!("Access to undeclared static property {}::${n}", c.name)))
    }

    fn eval_binary(&mut self, op: php_parser::ast::BinaryOp, l: &Expr, r: &Expr) -> RtResult<Value> {
        use php_parser::ast::BinaryOp as P;
        match op {
            P::BoolAnd => {
                let a = self.eval(l)?;
                if !a.to_bool(self)? {
                    return Ok(Value::Bool(false));
                }
                let b = self.eval(r)?;
                Ok(Value::Bool(b.to_bool(self)?))
            }
            P::BoolOr => {
                let a = self.eval(l)?;
                if a.to_bool(self)? {
                    return Ok(Value::Bool(true));
                }
                let b = self.eval(r)?;
                Ok(Value::Bool(b.to_bool(self)?))
            }
            P::Coalesce => {
                if let Some(v) = self.eval_quiet(l)? {
                    if !v.is_null() {
                        return Ok(v);
                    }
                }
                self.eval(r)
            }
            _ => {
                let a = self.eval(l)?;
                let b = self.eval(r)?;
                self.binop(op, &a, &b)
            }
        }
    }

    pub fn binop(&mut self, op: php_parser::ast::BinaryOp, a: &Value, b: &Value) -> RtResult<Value> {
        use php_parser::ast::BinaryOp as P;
        Ok(match op {
            P::Identical => Value::Bool(ops::is_identical(a, b)?),
            P::NotIdentical => Value::Bool(!ops::is_identical(a, b)?),
            P::Equal => Value::Bool(ops::is_equal(a, b, self)?),
            P::NotEqual => Value::Bool(!ops::is_equal(a, b, self)?),
            P::Less => Value::Bool(ops::is_smaller(a, b, self)?),
            P::LessEqual => Value::Bool(ops::is_smaller_or_equal(a, b, self)?),
            P::Greater => Value::Bool(ops::is_smaller(b, a, self)?),
            P::GreaterEqual => Value::Bool(ops::is_smaller_or_equal(b, a, self)?),
            P::Spaceship => ops::spaceship(a, b, self)?,
            other => {
                let rop = runtime_binop(other).ok_or_else(|| rt_err("bad op"))?;
                ops::binary_op(rop, a, b, self)?
            }
        })
    }

    pub fn cast(&mut self, kind: CastKind, v: Value) -> RtResult<Value> {
        Ok(match kind {
            CastKind::Int => Value::Int(v.to_long(self)?),
            CastKind::Float => Value::Float(v.to_double(self)?),
            CastKind::String => Value::Str(v.to_zstr(self)?),
            CastKind::Bool => Value::Bool(v.to_bool(self)?),
            CastKind::Array => match v.deref() {
                Value::Object(o) => {
                    if let Extra::Closure(_) = &*tobj(&o).extra.borrow() {
                        return Ok(Value::Array(ZArray::from_values([Value::Object(o.clone())])));
                    }
                    let t = self.object_properties(&o, PropertyPurpose::ArrayCast)?.unwrap_or_default();
                    Value::Array(ZArray::from_table(t.to_symtable()))
                }
                other => Value::Array(other.to_array(self)?),
            },
            CastKind::Object => v.to_object(self)?,
            CastKind::Unset => Value::Null,
        })
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    pub fn read_dim(&mut self, b: &Value, k: &Value, kind: DimKind) -> RtResult<Value> {
        match b.deref() {
            Value::Array(a) => {
                let key = to_key(self, k)?;
                match a.get(&key) {
                    Some(v) => Ok(v.deref()),
                    None => {
                        if kind != DimKind::Is {
                            self.emit(ErrorLevel::Warning, &format!("Undefined array key {}", key_display(&key)))?;
                        }
                        Ok(Value::Null)
                    }
                }
            }
            Value::Object(o) => self.obj_read_dim(&o, Some(k), kind),
            Value::Str(st) => {
                let n = match k.deref() {
                    Value::Int(n) => n,
                    other => other.to_long(self)?,
                };
                let len = st.len() as i64;
                let idx = if n < 0 { n + len } else { n };
                if idx < 0 || idx >= len {
                    if kind != DimKind::Is {
                        self.emit(ErrorLevel::Warning, &format!("Uninitialized string offset {n}"))?;
                    }
                    return Ok(Value::from(""));
                }
                Ok(Value::Str(ZStr::from(&st.as_bytes()[idx as usize..=idx as usize])))
            }
            Value::Null => {
                if kind != DimKind::Is {
                    self.emit(ErrorLevel::Warning, "Trying to access array offset on null")?;
                }
                Ok(Value::Null)
            }
            other => {
                if kind != DimKind::Is {
                    self.emit(
                        ErrorLevel::Warning,
                        &format!("Trying to access array offset on {}", other.value_name()),
                    )?;
                }
                Ok(Value::Null)
            }
        }
    }

    pub fn obj_read_dim(&mut self, o: &Obj, k: Option<&Value>, kind: DimKind) -> RtResult<Value> {
        if let Some(v) = php_spl::handlers::read_dimension(self, o, k, kind)? {
            return Ok(v);
        }
        if !tobj(o).class.is_a("ArrayAccess") {
            return Err(rt_err(format!("Cannot use object of type {} as array", o.class_name())));
        }
        php_spl::handlers::std_read_dimension(self, o, k, kind)
    }

    pub fn read_prop(&mut self, o: &Value, n: &str) -> RtResult<Value> {
        let Value::Object(o) = o.deref() else {
            self.emit(
                ErrorLevel::Warning,
                &format!("Attempt to read property \"{n}\" on {}", o.deref().value_name()),
            )?;
            return Ok(Value::Null);
        };
        if let Some(v) = php_spl::handlers::read_property(self, &o, &ZStr::from(n), DimKind::Read)? {
            return Ok(v);
        }
        self.std_read_prop(&o, n, false)
    }

    pub fn std_read_prop(&mut self, o: &Obj, n: &str, quiet: bool) -> RtResult<Value> {
        if let Some(k) = self.find_prop_key(o, n) {
            if self.prop_visible(o, &k) {
                return Ok(tobj(o).props.borrow().get_str(k.as_bytes()).map(Value::deref).unwrap_or_default());
            }
            if tobj(o).class.find_method("__get").is_none() {
                return Err(rt_err(format!(
                    "Cannot access {} property {}::${n}",
                    if k.as_bytes().get(1) == Some(&b'*') { "protected" } else { "private" },
                    o.class_name()
                )));
            }
        }
        if let Some(m) = tobj(o).class.find_method("__get") {
            let cls = tobj(o).class.clone();
            return self.call_method_h(Some(o), &cls, &m, &[Value::from(n)]);
        }
        if !quiet {
            self.emit(ErrorLevel::Warning, &format!("Undefined property: {}::${n}", o.class_name()))?;
        }
        Ok(Value::Null)
    }

    pub fn prop_visible(&self, o: &Obj, key: &ZStr) -> bool {
        let (cls, _) = php_runtime::dump::unmangle_property_name(key.as_bytes());
        let scope = self.frames.last().and_then(|f| f.scope.clone());
        match cls {
            None => true,
            Some(b"*") => scope.is_some_and(|sc| tobj(o).class.is_a(&sc.name) || sc.is_a(&tobj(o).class.name)),
            Some(c) => scope.is_some_and(|sc| sc.name.as_bytes() == c),
        }
    }

    pub fn write_prop(&mut self, o: &Obj, n: &str, v: Value) -> RtResult<()> {
        if php_spl::handlers::write_property(self, o, &ZStr::from(n), v.clone())? {
            return Ok(());
        }
        self.std_write_prop(o, n, v)
    }

    pub fn std_write_prop(&mut self, o: &Obj, n: &str, v: Value) -> RtResult<()> {
        if let Some(k) = self.find_prop_key(o, n) {
            if self.prop_visible(o, &k) || tobj(o).class.find_method("__set").is_none() {
                let t = tobj(o);
                let mut p = t.props.borrow_mut();
                let ht = p.make_mut();
                match ht.get_mut(&ArrayKey::Str(k.clone())) {
                    Some(Value::Reference(r)) => {
                        r.set(v);
                    }
                    _ => {
                        ht.insert_str_raw(k, v);
                    }
                }
                return Ok(());
            }
        }
        // declared typed props that are uninitialized
        let declared = {
            let cls = tobj(o).class.clone();
            cls.all_props().iter().find(|(k, i)| k.props[*i].name == n).map(|(k, i)| mangle(&k.name, n, k.props[*i].visibility))
        };
        if let Some(key) = declared {
            tobj(o).props.borrow_mut().make_mut().insert_str_raw(key, v);
            return Ok(());
        }
        if let Some(m) = tobj(o).class.find_method("__set") {
            let cls = tobj(o).class.clone();
            self.call_method_h(Some(o), &cls, &m, &[Value::from(n), v])?;
            return Ok(());
        }
        let cls = tobj(o).class.clone();
        if !cls.allow_dynamic && !cls.name.eq_ignore_ascii_case("stdClass") {
            self.emit(
                ErrorLevel::Deprecated,
                &format!("Creation of dynamic property {}::${n} is deprecated", cls.name),
            )?;
        }
        tobj(o).props.borrow_mut().make_mut().insert_str_raw(ZStr::from(n), v);
        Ok(())
    }

    /// Evaluates without "undefined" warnings (for `??` and `isset`).
    pub fn eval_quiet(&mut self, e: &Expr) -> RtResult<Option<Value>> {
        match &e.kind {
            ExprKind::Var(v) => {
                let n = self.var_name(v)?;
                if n == b"this" {
                    return Ok(self.frame().this.clone().map(Value::Object));
                }
                Ok(self.frame().vars.get(&n).map(Value::deref))
            }
            ExprKind::Dim { base, index: Some(index) } => {
                let Some(b) = self.eval_quiet(base)? else { return Ok(None) };
                let k = self.eval(index)?;
                match b.deref() {
                    Value::Array(a) => {
                        let key = to_key(self, &k)?;
                        Ok(a.get(&key).map(Value::deref))
                    }
                    Value::Object(o) => {
                        let v = self.obj_read_dim(&o, Some(&k), DimKind::Is)?;
                        Ok(Some(v))
                    }
                    Value::Str(_) => {
                        if self.isset_value_dim(&b, &k)? {
                            self.read_dim(&b, &k, DimKind::Is).map(Some)
                        } else {
                            Ok(None)
                        }
                    }
                    _ => Ok(None),
                }
            }
            ExprKind::Prop { object, name, .. } => {
                let Some(o) = self.eval_quiet(object)? else { return Ok(None) };
                let n = self.member_name(name)?;
                let Value::Object(o) = o.deref() else { return Ok(None) };
                if !self.prop_isset(&o, &n)? {
                    return Ok(None);
                }
                if let Some(v) = php_spl::handlers::read_property(self, &o, &ZStr::from(n.as_str()), DimKind::Is)? {
                    return Ok(Some(v));
                }
                self.std_read_prop(&o, &n, true).map(Some)
            }
            ExprKind::StaticProp { class, name, .. } => {
                let c = self.resolve_class_ref(class)?;
                let n = s(&self.var_name(name)?);
                Ok(self.static_prop(&c, &n).ok().map(|r| r.get()))
            }
            ExprKind::ClassConst { .. } | ExprKind::Const(_) => {
                Ok(Some(self.eval(e)?))
            }
            _ => self.eval(e).map(Some),
        }
    }

    fn isset_value_dim(&mut self, b: &Value, k: &Value) -> RtResult<bool> {
        match b.deref() {
            Value::Array(a) => {
                let key = to_key(self, k)?;
                Ok(a.get(&key).is_some_and(|v| !v.is_null()))
            }
            Value::Object(o) => self.obj_has_dim(&o, k, false),
            Value::Str(st) => {
                let n = match k.deref() {
                    Value::Int(n) => n,
                    Value::Str(ks) => match php_runtime::numeric::is_numeric_string(ks.as_bytes(), false) {
                        Some(php_runtime::numeric::Number::Int(n)) => n,
                        _ => return Ok(false),
                    },
                    Value::Float(f) => f as i64,
                    Value::Bool(b) => i64::from(b),
                    _ => return Ok(false),
                };
                let len = st.len() as i64;
                Ok((n >= 0 && n < len) || (n < 0 && -n <= len))
            }
            _ => Ok(false),
        }
    }

    pub fn obj_has_dim(&mut self, o: &Obj, k: &Value, check_empty: bool) -> RtResult<bool> {
        if let Some(r) = php_spl::handlers::has_dimension(self, o, k, check_empty)? {
            return Ok(r);
        }
        if !tobj(o).class.is_a("ArrayAccess") {
            return Err(rt_err(format!("Cannot use object of type {} as array", o.class_name())));
        }
        php_spl::handlers::std_has_dimension(self, o, k, check_empty)
    }

    fn prop_isset(&mut self, o: &Obj, n: &str) -> RtResult<bool> {
        if let Some(r) = php_spl::handlers::has_property(self, o, &ZStr::from(n), 0)? {
            return Ok(r);
        }
        if let Some(k) = self.find_prop_key(o, n) {
            if self.prop_visible(o, &k) {
                return Ok(tobj(o).props.borrow().get_str(k.as_bytes()).is_some_and(|v| !v.is_null()));
            }
        }
        if let Some(m) = tobj(o).class.find_method("__isset") {
            let cls = tobj(o).class.clone();
            let r = self.call_method_h(Some(o), &cls, &m, &[Value::from(n)])?;
            return r.to_bool(self);
        }
        Ok(false)
    }

    pub fn eval_isset(&mut self, e: &Expr) -> RtResult<bool> {
        match &e.kind {
            ExprKind::Dim { base, index: Some(index) } => {
                let Some(b) = self.eval_quiet(base)? else { return Ok(false) };
                let k = self.eval(index)?;
                self.isset_value_dim(&b, &k)
            }
            ExprKind::Prop { object, name, .. } => {
                let Some(o) = self.eval_quiet(object)? else { return Ok(false) };
                let n = self.member_name(name)?;
                match o.deref() {
                    Value::Object(o) => self.prop_isset(&o, &n),
                    _ => Ok(false),
                }
            }
            _ => Ok(self.eval_quiet(e)?.is_some_and(|v| !v.is_null())),
        }
    }

    fn eval_empty(&mut self, e: &Expr) -> RtResult<bool> {
        match &e.kind {
            ExprKind::Dim { base, index: Some(index) } => {
                let Some(b) = self.eval_quiet(base)? else { return Ok(true) };
                let k = self.eval(index)?;
                if let Value::Object(o) = b.deref() {
                    return Ok(!self.obj_has_dim(&o, &k, true)?);
                }
                if !self.isset_value_dim(&b, &k)? {
                    return Ok(true);
                }
                let v = self.read_dim(&b, &k, DimKind::Is)?;
                Ok(!v.to_bool(self)?)
            }
            ExprKind::Prop { object, name, .. } => {
                let Some(o) = self.eval_quiet(object)? else { return Ok(true) };
                let n = self.member_name(name)?;
                let Value::Object(o) = o.deref() else { return Ok(true) };
                if let Some(r) = php_spl::handlers::has_property(self, &o, &ZStr::from(n.as_str()), 1)? {
                    return Ok(!r);
                }
                if !self.prop_isset(&o, &n)? {
                    return Ok(true);
                }
                let v = self.std_read_prop(&o, &n, true)?;
                Ok(!v.to_bool(self)?)
            }
            _ => match self.eval_quiet(e)? {
                Some(v) => Ok(!v.to_bool(self)?),
                None => Ok(true),
            },
        }
    }

    // -----------------------------------------------------------------------
    // Writes
    // -----------------------------------------------------------------------

    pub fn assign(&mut self, target: &Expr, v: Value) -> RtResult<Value> {
        if let ExprKind::Array { items, .. } = &target.kind {
            self.destructure(items, &v)?;
            return Ok(v);
        }
        let (base, path) = self.lvalue(target)?;
        self.apply(base, &path, Op::Assign(v))
    }

    pub fn destructure(&mut self, items: &[Option<ArrayItem>], v: &Value) -> RtResult<()> {
        let mut idx = 0i64;
        for item in items {
            let Some(item) = item else {
                idx += 1;
                continue;
            };
            let key = match &item.key {
                Some(k) => self.eval(k)?,
                None => {
                    let k = Value::Int(idx);
                    idx += 1;
                    k
                }
            };
            let val = match v.deref() {
                Value::Array(_) | Value::Object(_) => self.read_dim(v, &key, DimKind::Read)?,
                _ => Value::Null,
            };
            if let ExprKind::Array { items: inner, .. } = &item.value.kind {
                self.destructure(inner, &val)?;
            } else {
                self.assign(&item.value, val)?;
            }
        }
        Ok(())
    }

    pub fn lvalue(&mut self, e: &Expr) -> RtResult<(LBase, Vec<Option<Value>>)> {
        match &e.kind {
            ExprKind::Var(v) => {
                let n = self.var_name(v)?;
                if n == b"this" {
                    let t = self.frame().this.clone().map(Value::Object).unwrap_or_default();
                    return Ok((LBase::Temp(t), Vec::new()));
                }
                Ok((LBase::Var(n), Vec::new()))
            }
            ExprKind::Dim { base, index } => {
                let (b, mut path) = self.lvalue(base)?;
                let k = match index {
                    Some(i) => Some(self.eval(i)?),
                    None => None,
                };
                path.push(k);
                Ok((b, path))
            }
            ExprKind::Prop { object, name, .. } => {
                let o = match &object.kind {
                    ExprKind::Var(VarName::Name(id)) if id.name == b"this" => {
                        self.frame().this.clone().map(Value::Object).unwrap_or_default()
                    }
                    _ => {
                        // auto-vivification of objects is an error in PHP 8
                        self.eval(object)?
                    }
                };
                let n = self.member_name(name)?;
                match o.deref() {
                    Value::Object(o) => Ok((LBase::Prop(o, n), Vec::new())),
                    other => Err(rt_err(format!("Attempt to assign property \"{n}\" on {}", other.value_name()))),
                }
            }
            ExprKind::StaticProp { class, name, .. } => {
                let c = self.resolve_class_ref(class)?;
                let n = s(&self.var_name(name)?);
                Ok((LBase::StaticProp(c, n), Vec::new()))
            }
            _ => {
                let v = self.eval(e)?;
                Ok((LBase::Temp(v), Vec::new()))
            }
        }
    }

    pub fn apply(&mut self, base: LBase, path: &[Option<Value>], op: Op) -> RtResult<Value> {
        match base {
            LBase::Var(n) => {
                if path.is_empty() {
                    match op {
                        Op::Unset => {
                            self.frame().vars.remove(&n);
                            return Ok(Value::Null);
                        }
                        Op::SetRef(r) => {
                            self.frame().vars.insert(n, Value::Reference(r.clone()));
                            return Ok(r.get());
                        }
                        Op::MakeRef => {
                            let r = self.frame().vars.entry(n).or_insert(Value::Null).make_ref();
                            return Ok(Value::Reference(r));
                        }
                        _ => {}
                    }
                }
                let exists = self.frame().vars.contains_key(&n);
                if !exists {
                    if matches!(op, Op::Unset) {
                        return Ok(Value::Null);
                    }
                    if path.is_empty() && matches!(op, Op::AssignOp(..) | Op::IncDec(_)) {
                        self.emit(ErrorLevel::Warning, &format!("Undefined variable ${}", s(&n)))?;
                    }
                }
                let slot = self.frame().vars.remove(&n).unwrap_or(Value::Null);
                if let Value::Reference(r) = &slot {
                    let mut inner = r.get();
                    let res = self.apply_val(&mut inner, path, op);
                    r.set(inner);
                    self.frame().vars.insert(n, slot);
                    return res;
                }
                let mut v = slot;
                let res = self.apply_val(&mut v, path, op);
                self.frame().vars.insert(n, v);
                res
            }
            LBase::Prop(o, n) => {
                if path.is_empty() {
                    match op {
                        Op::Assign(v) => {
                            self.write_prop(&o, &n, v.clone())?;
                            return Ok(v);
                        }
                        Op::Unset => {
                            self.unset_prop(&o, &n)?;
                            return Ok(Value::Null);
                        }
                        Op::SetRef(r) => {
                            let key = self.find_prop_key(&o, &n).unwrap_or_else(|| ZStr::from(n.as_str()));
                            tobj(&o).props.borrow_mut().make_mut().insert_str_raw(key, Value::Reference(r.clone()));
                            return Ok(r.get());
                        }
                        _ => {}
                    }
                }
                // ArrayObject with ARRAY_AS_PROPS etc.
                if let Some(v) = php_spl::handlers::read_property(self, &o, &ZStr::from(n.as_str()), DimKind::Write)? {
                    let mut v = v;
                    let res = self.apply_val(&mut v, path, op);
                    if !matches!(v, Value::Reference(_) | Value::Object(_)) {
                        php_spl::handlers::write_property(self, &o, &ZStr::from(n.as_str()), v)?;
                    }
                    return res;
                }
                let key = match self.find_prop_key(&o, &n) {
                    Some(k) => k,
                    None => {
                        if let Some(m) = tobj(&o).class.find_method("__get") {
                            let cls = tobj(&o).class.clone();
                            let mut v = self.call_method_h(Some(&o), &cls, &m, &[Value::from(n.as_str())])?;
                            if !matches!(v, Value::Object(_) | Value::Reference(_)) && !path.is_empty() {
                                self.emit(
                                    ErrorLevel::Notice,
                                    &format!("Indirect modification of overloaded property {}::${n} has no effect", o.class_name()),
                                )?;
                            }
                            return self.apply_val(&mut v, path, op);
                        }
                        if matches!(op, Op::AssignOp(..) | Op::IncDec(_)) && path.is_empty() {
                            self.emit(ErrorLevel::Warning, &format!("Undefined property: {}::${n}", o.class_name()))?;
                        }
                        self.std_write_prop(&o, &n, Value::Null)?;
                        self.find_prop_key(&o, &n).unwrap_or_else(|| ZStr::from(n.as_str()))
                    }
                };
                let cur = {
                    let t = tobj(&o);
                    let mut p = t.props.borrow_mut();
                    let ht = p.make_mut();
                    match ht.get_mut(&ArrayKey::Str(key.clone())) {
                        Some(v) => std::mem::take(v),
                        None => Value::Null,
                    }
                };
                if let Value::Reference(r) = &cur {
                    let mut inner = r.get();
                    let res = self.apply_val(&mut inner, path, op);
                    r.set(inner);
                    tobj(&o).props.borrow_mut().make_mut().insert_str_raw(key, cur);
                    return res;
                }
                let mut v = cur;
                let res = self.apply_val(&mut v, path, op);
                tobj(&o).props.borrow_mut().make_mut().insert_str_raw(key, v);
                res
            }
            LBase::StaticProp(c, n) => {
                let r = self.static_prop(&c, &n)?;
                if path.is_empty() {
                    if let Op::SetRef(nr) = &op {
                        let mut k = Some(c.clone());
                        while let Some(cl) = k {
                            if cl.statics.borrow().contains_key(&n) {
                                cl.statics.borrow_mut().insert(n.clone(), Value::Reference(nr.clone()));
                                break;
                            }
                            k = cl.parent.clone();
                        }
                        return Ok(nr.get());
                    }
                }
                let mut inner = r.get();
                let res = self.apply_val(&mut inner, path, op);
                r.set(inner);
                res
            }
            LBase::Temp(mut v) => {
                if path.is_empty() {
                    return Err(rt_err("Cannot assign to temporary expression"));
                }
                self.apply_val(&mut v, path, op)
            }
        }
    }

    fn apply_val(&mut self, v: &mut Value, path: &[Option<Value>], op: Op) -> RtResult<Value> {
        if path.is_empty() {
            return self.apply_leaf(v, op);
        }
        if let Value::Reference(r) = v {
            let r = r.clone();
            let mut inner = r.get();
            let res = self.apply_val(&mut inner, path, op);
            r.set(inner);
            return res;
        }
        match v {
            Value::Null => {
                if matches!(op, Op::Unset) {
                    return Ok(Value::Null);
                }
                *v = Value::empty_array();
            }
            Value::Bool(false) => {
                if matches!(op, Op::Unset) {
                    return Ok(Value::Null);
                }
                self.emit(ErrorLevel::Deprecated, "Automatic conversion of false to array is deprecated")?;
                *v = Value::empty_array();
            }
            _ => {}
        }
        match v {
            Value::Array(a) => {
                let key = match &path[0] {
                    Some(k) => Some(to_key(self, k)?),
                    None => None,
                };
                if path.len() == 1 {
                    match op {
                        Op::Unset => {
                            if let Some(k) = key {
                                a.make_mut().remove(&k);
                            }
                            return Ok(Value::Null);
                        }
                        Op::Assign(val) => {
                            let ht = a.make_mut();
                            match key {
                                Some(k) => match ht.get_mut(&k) {
                                    Some(Value::Reference(r)) => {
                                        r.set(val.clone());
                                    }
                                    _ => {
                                        ht.insert(k, val.clone());
                                    }
                                },
                                None => {
                                    if ht.push(val.clone()).is_err() {
                                        return Err(rt_err("Cannot add element to the array as the next element is already occupied"));
                                    }
                                }
                            }
                            return Ok(val);
                        }
                        _ => {}
                    }
                }
                let ht = a.make_mut();
                let (mut slot, existed) = match &key {
                    Some(k) => match ht.get_mut(k) {
                        Some(s) => (std::mem::take(s), true),
                        None => (Value::Null, false),
                    },
                    None => (Value::Null, false),
                };
                if !existed && path.len() == 1 && matches!(op, Op::AssignOp(..) | Op::IncDec(_)) {
                    if let Some(k) = &key {
                        self.emit(ErrorLevel::Warning, &format!("Undefined array key {}", key_display(k)))?;
                    }
                }
                let res = self.apply_val(&mut slot, &path[1..], op);
                let Value::Array(a) = v else { unreachable!() };
                let ht = a.make_mut();
                match key {
                    Some(k) => {
                        ht.insert(k, slot);
                    }
                    None => {
                        let _ = ht.push(slot);
                    }
                }
                res
            }
            Value::Object(o) => {
                let o = o.clone();
                let key = path[0].clone();
                if path.len() == 1 {
                    return match op {
                        Op::Assign(val) => {
                            self.obj_write_dim(&o, key.as_ref(), val.clone())?;
                            Ok(val)
                        }
                        Op::Unset => {
                            let k = key.unwrap_or_default();
                            if !php_spl::handlers::unset_dimension(self, &o, &k)? {
                                self.call_method(&o, "offsetUnset", &[k])?;
                            }
                            Ok(Value::Null)
                        }
                        Op::AssignOp(bop, rhs) => {
                            let cur = self.obj_read_dim(&o, key.as_ref(), DimKind::Read)?.deref();
                            let nv = self.binop(bop, &cur, &rhs)?;
                            self.obj_write_dim(&o, key.as_ref(), nv.clone())?;
                            Ok(nv)
                        }
                        Op::IncDec(idop) => {
                            let cur = self.obj_read_dim(&o, key.as_ref(), DimKind::ReadWrite)?;
                            let mut nv = cur.deref();
                            let old = nv.clone();
                            match idop {
                                IncDecOp::PreInc | IncDecOp::PostInc => ops::increment(&mut nv, self)?,
                                _ => ops::decrement(&mut nv, self)?,
                            }
                            self.obj_write_dim(&o, key.as_ref(), nv.clone())?;
                            Ok(if matches!(idop, IncDecOp::PostInc | IncDecOp::PostDec) { old } else { nv })
                        }
                        Op::MakeRef | Op::SetRef(_) => {
                            let cur = self.obj_read_dim(&o, key.as_ref(), DimKind::Write)?;
                            match cur {
                                Value::Reference(r) => {
                                    if let Op::SetRef(nr) = op {
                                        r.set(nr.get());
                                    }
                                    Ok(Value::Reference(r))
                                }
                                other => {
                                    self.emit(
                                        ErrorLevel::Notice,
                                        &format!("Indirect modification of overloaded element of {} has no effect", o.class_name()),
                                    )?;
                                    Ok(Value::Reference(PhpRef::new(other)))
                                }
                            }
                        }
                    };
                }
                let mut cur = self.obj_read_dim(&o, key.as_ref(), DimKind::Write)?;
                if !matches!(cur, Value::Reference(_) | Value::Object(_)) {
                    self.emit(
                        ErrorLevel::Notice,
                        &format!("Indirect modification of overloaded element of {} has no effect", o.class_name()),
                    )?;
                }
                self.apply_val(&mut cur, &path[1..], op)
            }
            Value::Str(_) => Err(rt_err("String offset writes unsupported in harness")),
            other => {
                if matches!(op, Op::Unset) {
                    return Ok(Value::Null);
                }
                Err(rt_err(format!("Cannot use a scalar value as an array ({})", other.type_name())))
            }
        }
    }

    fn apply_leaf(&mut self, v: &mut Value, op: Op) -> RtResult<Value> {
        match op {
            Op::Assign(val) => {
                match v {
                    Value::Reference(r) => {
                        r.set(val.clone());
                    }
                    _ => *v = val.clone(),
                }
                Ok(val)
            }
            Op::AssignOp(bop, rhs) => {
                let cur = v.deref();
                let nv = self.binop(bop, &cur, &rhs)?;
                match v {
                    Value::Reference(r) => {
                        r.set(nv.clone());
                    }
                    _ => *v = nv.clone(),
                }
                Ok(nv)
            }
            Op::IncDec(idop) => {
                let mut nv = v.deref();
                let old = nv.clone();
                match idop {
                    IncDecOp::PreInc | IncDecOp::PostInc => ops::increment(&mut nv, self)?,
                    _ => ops::decrement(&mut nv, self)?,
                }
                match v {
                    Value::Reference(r) => {
                        r.set(nv.clone());
                    }
                    _ => *v = nv.clone(),
                }
                Ok(if matches!(idop, IncDecOp::PostInc | IncDecOp::PostDec) { old } else { nv })
            }
            Op::MakeRef => Ok(Value::Reference(v.make_ref())),
            Op::SetRef(r) => {
                *v = Value::Reference(r.clone());
                Ok(r.get())
            }
            Op::Unset => Ok(Value::Null),
        }
    }

    pub fn obj_write_dim(&mut self, o: &Obj, k: Option<&Value>, v: Value) -> RtResult<()> {
        if php_spl::handlers::write_dimension(self, o, k, v.clone())? {
            return Ok(());
        }
        if !tobj(o).class.is_a("ArrayAccess") {
            return Err(rt_err(format!("Cannot use object of type {} as array", o.class_name())));
        }
        self.call_method(o, "offsetSet", &[k.cloned().unwrap_or_default(), v])?;
        Ok(())
    }

    pub fn get_ref(&mut self, e: &Expr) -> RtResult<PhpRef> {
        let (base, path) = self.lvalue(e)?;
        match self.apply(base, &path, Op::MakeRef)? {
            Value::Reference(r) => Ok(r),
            other => Ok(PhpRef::new(other)),
        }
    }

    pub fn assign_ref_to(&mut self, target: &Expr, r: PhpRef) -> RtResult<()> {
        let (base, path) = self.lvalue(target)?;
        self.apply(base, &path, Op::SetRef(r))?;
        Ok(())
    }

    pub fn unset(&mut self, e: &Expr) -> RtResult<()> {
        let (base, path) = self.lvalue(e)?;
        self.apply(base, &path, Op::Unset)?;
        Ok(())
    }

    fn unset_prop(&mut self, o: &Obj, n: &str) -> RtResult<()> {
        if php_spl::handlers::unset_property(self, o, &ZStr::from(n))? {
            return Ok(());
        }
        if let Some(k) = self.find_prop_key(o, n) {
            tobj(o).props.borrow_mut().make_mut().remove(&ArrayKey::Str(k));
            return Ok(());
        }
        if let Some(m) = tobj(o).class.find_method("__unset") {
            let cls = tobj(o).class.clone();
            self.call_method_h(Some(o), &cls, &m, &[Value::from(n)])?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Calls
    // -----------------------------------------------------------------------

    pub fn eval_args(&mut self, args: &ArgList, by_ref: &dyn Fn(usize) -> bool) -> RtResult<Vec<Value>> {
        self.pending_named.clear();
        let mut out = Vec::new();
        for a in &args.args {
            if let ArgKind::Named { name, value } = &a.kind {
                let v = self.eval(value)?.deref();
                self.pending_named.push((s(&name.name), v));
                continue;
            }
            match &a.kind {
                ArgKind::Positional(e) => {
                    let i = out.len();
                    if by_ref(i) && is_lvalue(e) {
                        out.push(Value::Reference(self.get_ref(e)?));
                    } else {
                        out.push(self.eval(e)?.deref());
                    }
                }
                ArgKind::Named { .. } => {}
                ArgKind::Unpack(e) => {
                    let v = self.eval(e)?;
                    match v {
                        Value::Array(a) => out.extend(a.values().map(Value::deref)),
                        Value::Object(o) => {
                            let mut it = php_spl::iter::get_iterator(self, &o, false)?;
                            it.rewind(self)?;
                            while it.valid(self)? {
                                let Some(v) = it.current(self)? else { break };
                                out.push(v);
                                it.next(self)?;
                            }
                        }
                        _ => return Err(rt_err("Only arrays and Traversables can be unpacked")),
                    }
                }
                _ => return Err(rt_err("partial application unsupported")),
            }
        }
        Ok(out)
    }

    fn ctor_by_ref(&self, c: &Rc<Class>) -> Vec<bool> {
        match c.find_method("__construct") {
            Some(m) => match &m.kind {
                MKind::User(d) => d.params.iter().map(|p| p.by_ref).collect(),
                _ => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    fn eval_call(&mut self, callee: &Callee, args: &ArgList) -> RtResult<Value> {
        match callee {
            Callee::Name(n) => {
                let raw = n.name.strip_prefix(b"\\").unwrap_or(&n.name);
                let name = lc(raw);
                let name = name.rsplit('\\').next().unwrap_or(&name).to_string();
                if args.is_first_class_callable() {
                    if let Some(f) = self.functions.get(&name).cloned() {
                        return self.make_closure(ClosureData { func: f, bound: Vec::new(), this: None, scope: None, builtin: None, method: None });
                    }
                    let def = Rc::new(FuncDef { return_type: None, name: name.clone(),
                        params: Vec::new(),
                        body: Rc::new(Vec::new()),
                        arrow: None,
                        by_ref: false,
                        is_generator: false,
                        statics: Rc::new(RefCell::new(HashMap::new())),
                    });
                    return self.make_closure(ClosureData { func: def, bound: Vec::new(), this: None, scope: None, builtin: Some(name), method: None });
                }
                if let Some(f) = self.functions.get(&name).cloned() {
                    let by_ref: Vec<bool> = f.params.iter().map(|p| p.by_ref).collect();
                    let argv = self.eval_args(args, &|i| by_ref.get(i).copied().unwrap_or(false))?;
                    return self.call_user(&f, &argv, None, None, None, &[]);
                }
                let argv = self.eval_args(args, &|i| crate::harness::builtins::by_ref(&name, i))?;
                let line = self.line;
                let r = crate::harness::builtins::call(self, &name, &argv);
                self.line = line;
                r
            }
            Callee::Expr(e) => {
                let f = self.eval(e)?;
                let by_ref = self.callable_by_ref(&f);
                let argv = self.eval_args(args, &|i| by_ref.get(i).copied().unwrap_or(false))?;
                self.call_value(&f, &argv)
            }
        }
    }

    fn callable_by_ref(&mut self, f: &Value) -> Vec<bool> {
        if let Value::Object(o) = f {
            if let Extra::Closure(c) = &*tobj(o).extra.borrow() {
                return c.func.params.iter().map(|p| p.by_ref).collect();
            }
        }
        Vec::new()
    }

    pub fn eval_method_call(&mut self, o: Value, n: &str, args: &ArgList) -> RtResult<Value> {
        let Value::Object(o) = o.deref() else {
            return Err(rt_err(format!("Call to a member function {n}() on {}", o.deref().value_name())));
        };
        let cls = tobj(&o).class.clone();
        php_spl::handlers::before_method_call(self, &o)?;
        let m = cls.find_method(&n.to_ascii_lowercase());
        if args.is_first_class_callable() {
            let m = m.ok_or_else(|| rt_err(format!("Call to undefined method {}::{n}()", cls.name)))?;
            let def = match &m.kind {
                MKind::User(d) => d.clone(),
                _ => Rc::new(FuncDef { return_type: None, name: n.into(), params: Vec::new(), body: Rc::new(Vec::new()), arrow: None, by_ref: false, is_generator: false, statics: Rc::new(RefCell::new(HashMap::new())) }),
            };
            return self.make_closure(ClosureData { func: def, bound: Vec::new(), this: Some(o.clone()), scope: Some(cls.clone()), builtin: None, method: Some((Some(o.clone()), cls, m)) });
        }
        let by_ref: Vec<bool> = match &m {
            Some(m) => match &m.kind {
                MKind::User(d) => d.params.iter().map(|p| p.by_ref).collect(),
                _ => Vec::new(),
            },
            None => self.callable_by_ref(&Value::Object(o.clone())),
        };
        let argv = self.eval_args(args, &|i| by_ref.get(i).copied().unwrap_or(false))?;
        match m {
            Some(m) => {
                if m.visibility == 2 {
                    let scope = self.frame().scope.clone();
                    if !scope.is_some_and(|s| s.name == m.class) {
                        return Err(rt_err(format!(
                            "Call to private method {}::{}() from {}",
                            cls.name,
                            m.name,
                            self.scope_desc()
                        )));
                    }
                } else if m.visibility == 1 {
                    let scope = self.frame().scope.clone();
                    if !scope.is_some_and(|s| s.is_a(&m.class) || cls.is_a(&s.name)) {
                        return Err(rt_err(format!(
                            "Call to protected method {}::{}() from {}",
                            cls.name,
                            m.name,
                            self.scope_desc()
                        )));
                    }
                }
                self.call_method_h(Some(&o), &cls, &m, &argv)
            }
            None => {
                if let Extra::Closure(_) = &*tobj(&o).extra.borrow() {
                    if n.eq_ignore_ascii_case("__invoke") {
                        return self.call_value(&Value::Object(o.clone()), &argv);
                    }
                }
                if let Some(call) = cls.find_method("__call") {
                    let a = Value::Array(ZArray::from_values(argv));
                    return self.call_method_h(Some(&o), &cls, &call, &[Value::from(n), a]);
                }
                if let Some(target) = php_spl::handlers::method_target(self, &o)? {
                    return self.call_method(&target, n, &argv);
                }
                Err(rt_err(format!("Call to undefined method {}::{n}()", cls.name)))
            }
        }
    }

    fn scope_desc(&mut self) -> String {
        match self.frame().scope.clone() {
            Some(s) => format!("scope {}", s.name),
            None => "global scope".into(),
        }
    }

    fn eval_static_call(&mut self, class: &ClassRef, n: &str, args: &ArgList) -> RtResult<Value> {
        let is_relative = matches!(class, ClassRef::Name(nm) if matches!(lc(&nm.name).as_str(), "parent" | "self" | "static"));
        let c = self.resolve_class_ref(class)?;
        let m = c
            .find_method(&n.to_ascii_lowercase())
            .ok_or_else(|| rt_err(format!("Call to undefined method {}::{n}()", c.name)))?;
        let by_ref: Vec<bool> = match &m.kind {
            MKind::User(d) => d.params.iter().map(|p| p.by_ref).collect(),
            _ => Vec::new(),
        };
        let argv = self.eval_args(args, &|i| by_ref.get(i).copied().unwrap_or(false))?;
        if m.is_static {
            let st = if is_relative { self.frame().static_scope.clone().unwrap_or(c.clone()) } else { c.clone() };
            return self.call_method_h(None, &st, &m, &argv);
        }
        let this = self.frame().this.clone();
        match this {
            Some(t) if tobj(&t).class.is_a(&c.name) => {
                let cls = tobj(&t).class.clone();
                self.call_method_h(Some(&t), &cls, &m, &argv)
            }
            _ => Err(rt_err(format!("Non-static method {}::{}() cannot be called statically", m.class, m.name))),
        }
    }

    /// Calls a resolved method. `cls` is the called class (`static`).
    pub fn call_method_h(&mut self, this: Option<&Obj>, cls: &Rc<Class>, m: &Rc<MethodH>, args: &[Value]) -> RtResult<Value> {
        if matches!(m.kind, MKind::Spl) {
            if let Some(msg) = php_spl::classes::deprecation_message(&m.class, &m.name) {
                self.emit(ErrorLevel::Deprecated, &msg)?;
            }
        }
        self.steps += 1;
        if self.steps % 1024 == 0 && std::time::Instant::now() > self.deadline {
            return Err(RuntimeError::Fatal("harness timeout".into()));
        }
        self.depth += 1;
        if self.depth > 3000 {
            self.depth -= 1;
            return Err(RuntimeError::Error("Maximum call stack size reached. Infinite recursion?".into()));
        }
        let r = self.call_method_h_inner(this, cls, m, args);
        self.depth -= 1;
        r
    }

    fn call_method_h_inner(&mut self, this: Option<&Obj>, cls: &Rc<Class>, m: &Rc<MethodH>, args: &[Value]) -> RtResult<Value> {
        let named = std::mem::take(&mut self.pending_named);
        let placed;
        let args = if named.is_empty() {
            args
        } else {
            placed = self.place_named(m, args, named)?;
            &placed[..]
        };
        match &m.kind {
            MKind::User(def) => {
                let scope = self.find_class(m.class.as_bytes()).or_else(|| Some(cls.clone()));
                let def = def.clone();
                self.call_user(&def, args, this.cloned(), scope, Some(cls.clone()), &[])
            }
            MKind::Spl => {
                let line = self.line;
                let r = php_spl::call_method(self, &m.class, &m.name, this, args);
                self.line = line;
                r
            }
            MKind::Native(f) => f(self, this, args),
            MKind::Abstract => Err(rt_err(format!("Cannot call abstract method {}::{}()", m.class, m.name))),
        }
    }

    /// Places named arguments by parameter name, filling gaps with defaults.
    fn place_named(&mut self, m: &Rc<MethodH>, args: &[Value], named: Vec<(String, Value)>) -> RtResult<Vec<Value>> {
        let params: Vec<(String, Option<Value>)> = match &m.kind {
            MKind::User(d) => {
                let mut v = Vec::new();
                for p in &d.params {
                    let def = match &p.default {
                        Some(e) => Some(self.eval(e)?),
                        None => None,
                    };
                    v.push((s(&p.name.name), def));
                }
                v
            }
            _ => match php_spl::classes::find_method(&m.class, &m.name) {
                Some((_, md)) => {
                    let mut v = Vec::new();
                    for p in md.params {
                        let def = match p.default {
                            Some(src) => Some(self.eval_source_expr(src)?),
                            None => None,
                        };
                        v.push((p.name.to_string(), def));
                    }
                    v
                }
                None => Vec::new(),
            },
        };
        let mut out: Vec<Option<Value>> = args.iter().cloned().map(Some).collect();
        for (n, v) in named {
            let Some(i) = params.iter().position(|(pn, _)| *pn == n) else {
                return Err(rt_err(format!("Unknown named parameter ${n}")));
            };
            if out.len() <= i {
                out.resize(i + 1, None);
            }
            out[i] = Some(v);
        }
        let mut res = Vec::new();
        for (i, v) in out.into_iter().enumerate() {
            match v {
                Some(v) => res.push(v),
                None => res.push(params.get(i).and_then(|p| p.1.clone()).unwrap_or_default()),
            }
        }
        Ok(res)
    }

    fn eval_source_expr(&mut self, src: &str) -> RtResult<Value> {
        let code = format!("<?php return {src};");
        let prog = php_parser::parse(code.as_bytes(), &php_parser::ParseOptions::default())
            .map_err(|e| rt_err(e.message.clone()))?;
        match prog.statements.first().map(|s| &s.kind) {
            Some(StmtKind::Return(Some(e))) => {
                let e = e.clone();
                self.eval(&e)
            }
            _ => Ok(Value::Null),
        }
    }

    pub fn call_method(&mut self, o: &Obj, name: &str, args: &[Value]) -> RtResult<Value> {
        let cls = tobj(o).class.clone();
        match cls.find_method(&name.to_ascii_lowercase()) {
            Some(m) => self.call_method_h(Some(o), &cls, &m, args),
            None => {
                if let Some(call) = cls.find_method("__call") {
                    let a = Value::Array(ZArray::from_values(args.iter().cloned()));
                    return self.call_method_h(Some(o), &cls, &call, &[Value::from(name), a]);
                }
                Err(rt_err(format!("Call to undefined method {}::{name}()", cls.name)))
            }
        }
    }

    pub fn call_user(
        &mut self,
        def: &Rc<FuncDef>,
        args: &[Value],
        this: Option<Obj>,
        scope: Option<Rc<Class>>,
        static_scope: Option<Rc<Class>>,
        bound: &[(Vec<u8>, Value)],
    ) -> RtResult<Value> {
        if def.is_generator {
            return Err(rt_err("generators unsupported"));
        }
        self.depth += 1;
        if self.depth > 2000 {
            self.depth -= 1;
            return Err(RuntimeError::Fatal("Maximum call stack size reached".into()));
        }
        let mut frame = Frame {
            vars: HashMap::new(),
            this,
            scope,
            static_scope,
            func: def.name.clone(),
            args: args.to_vec(),
            func_def: Some(def.clone()),
        };
        for (k, v) in bound {
            frame.vars.insert(k.clone(), v.clone());
        }
        let line = self.line;
        self.frames.push(frame);
        let r = (|| -> RtResult<Value> {
            let required = def.params.iter().take_while(|p| p.default.is_none() && !p.variadic).count();
            let required = def.params.iter().rposition(|p| p.default.is_none() && !p.variadic).map_or(0, |i| i + 1).max(required);
            if args.len() < required {
                let exact = def.params.iter().all(|p| p.default.is_none() && !p.variadic);
                return Err(RuntimeError::ArgumentCountError(format!(
                    "Too few arguments to function {}(), {} passed in {} on line {} and {} {} expected",
                    def.name,
                    args.len(),
                    self.file,
                    line,
                    if exact { "exactly" } else { "at least" },
                    required
                )));
            }
            for (i, p) in def.params.iter().enumerate() {
                let v = if p.variadic {
                    Value::Array(ZArray::from_values(args.iter().skip(i).cloned()))
                } else if i < args.len() {
                    if p.by_ref {
                        match &args[i] {
                            Value::Reference(r) => Value::Reference(r.clone()),
                            other => other.clone(),
                        }
                    } else {
                        let v = args[i].deref();
                        self.coerce_param(def, i, p, v)?
                    }
                } else {
                    match &p.default {
                        Some(d) => self.eval(d)?,
                        None => Value::Null,
                    }
                };
                self.frame().vars.insert(p.name.name.clone(), v);
                if !p.modifiers.is_empty() {
                    if let Some(t) = self.frame().this.clone() {
                        let val = self.frame().vars.get(&p.name.name).map(Value::deref).unwrap_or_default();
                        self.prop_set_raw(&t, &s(&p.name.name), val);
                    }
                }
            }
            if let Some(a) = &def.arrow {
                return self.eval(a);
            }
            match self.exec_block(&def.body)? {
                Flow::Return(v) => self.coerce_return(def, v),
                _ => Ok(Value::Null),
            }
        })();
        self.frames.pop();
        self.depth -= 1;
        self.line = line;
        r
    }

    fn coerce_return(&mut self, def: &FuncDef, v: Value) -> RtResult<Value> {
        let Some(rt) = &def.return_type else { return Ok(v) };
        let TypeKind::Named(n) = &rt.kind else { return Ok(v) };
        Ok(match (lc(&n.name).as_str(), v.deref()) {
            ("string", Value::Int(_) | Value::Float(_)) => Value::Str(v.to_zstr(self)?),
            ("int", Value::Float(f)) if f.fract() == 0.0 => Value::Int(f as i64),
            ("float", Value::Int(i)) => Value::Float(i as f64),
            ("int", Value::Bool(b)) => Value::Int(i64::from(b)),
            _ => v,
        })
    }

    fn coerce_param(&mut self, def: &FuncDef, i: usize, p: &Param, v: Value) -> RtResult<Value> {
        let Some(ty) = &p.ty else { return Ok(v) };
        let (nullable, kind) = match &ty.kind {
            TypeKind::Nullable(inner) => (true, &inner.kind),
            k => (false, k),
        };
        let TypeKind::Named(n) = kind else { return Ok(v) };
        let tn = lc(&n.name);
        if nullable && v.is_null() {
            return Ok(v);
        }
        let ok = match (tn.as_str(), &v) {
            ("int", Value::Int(_)) | ("float", Value::Float(_)) | ("string", Value::Str(_)) | ("bool", Value::Bool(_)) => true,
            ("float", Value::Int(n)) => return Ok(Value::Float(*n as f64)),
            ("int", Value::Float(f)) if f.fract() == 0.0 => return Ok(Value::Int(*f as i64)),
            ("int", Value::Str(st)) => match php_runtime::numeric::is_numeric_string(st.as_bytes(), false) {
                Some(php_runtime::numeric::Number::Int(n)) => return Ok(Value::Int(n)),
                _ => false,
            },
            ("string", Value::Int(_) | Value::Float(_)) => return Ok(Value::Str(v.to_zstr(self)?)),
            ("int" | "float" | "string" | "bool", _) => false,
            _ => true,
        };
        if ok {
            return Ok(v);
        }
        Err(RuntimeError::TypeError(format!(
            "{}(): Argument #{} (${}) must be of type {}{}, {} given, called in {} on line {}",
            def.name,
            i + 1,
            s(&p.name.name),
            if nullable { "?" } else { "" },
            tn,
            v.value_name(),
            self.file,
            self.line
        )))
    }

    /// Calls a PHP callable value.
    pub fn call_value(&mut self, f: &Value, args: &[Value]) -> RtResult<Value> {
        match f.deref() {
            Value::Str(name) => {
                let n = name.to_string_lossy();
                let n = n.trim_start_matches('\\').to_string();
                if let Some((c, m)) = n.split_once("::") {
                    let cls = self.lookup_class(c.as_bytes())?;
                    let mh = cls
                        .find_method(&m.to_ascii_lowercase())
                        .ok_or_else(|| rt_err(format!("Call to undefined method {}::{m}()", cls.name)))?;
                    let this = if mh.is_static { None } else { self.frame().this.clone() };
                    return self.call_method_h(this.as_ref(), &cls, &mh, args);
                }
                let l = n.to_ascii_lowercase();
                if let Some(def) = self.functions.get(&l).cloned() {
                    return self.call_user(&def, args, None, None, None, &[]);
                }
                crate::harness::builtins::call(self, &l, args)
            }
            Value::Array(a) if a.len() == 2 => {
                let target = a.get_int(0).map(Value::deref).unwrap_or_default();
                let m = a.get_int(1).map(Value::deref).unwrap_or_default().to_zstr(self)?.to_string_lossy();
                match target {
                    Value::Object(o) => {
                        let mut cls = tobj(&o).class.clone();
                        let mut mname = m.clone();
                        if let Some(rest) = m.strip_prefix("parent::") {
                            cls = cls.parent.clone().ok_or_else(|| rt_err("no parent"))?;
                            mname = rest.to_string();
                        }
                        match cls.find_method(&mname.to_ascii_lowercase()) {
                            Some(mh) => {
                                let called = tobj(&o).class.clone();
                                self.call_method_h(Some(&o), &called, &mh, args)
                            }
                            None => self.call_method(&o, &mname, args),
                        }
                    }
                    other => {
                        let cn = other.to_zstr(self)?;
                        let cls = self.lookup_class(cn.as_bytes())?;
                        let mut mname = m.clone();
                        let mut lookup = cls.clone();
                        if let Some(rest) = m.strip_prefix("parent::") {
                            lookup = cls.parent.clone().ok_or_else(|| rt_err("no parent"))?;
                            mname = rest.to_string();
                        }
                        let mh = lookup
                            .find_method(&mname.to_ascii_lowercase())
                            .ok_or_else(|| rt_err(format!("Call to undefined method {}::{mname}()", cls.name)))?;
                        let this = if mh.is_static { None } else { self.frame().this.clone() };
                        self.call_method_h(this.as_ref(), &cls, &mh, args)
                    }
                }
            }
            Value::Object(o) => {
                let cd = match &*tobj(&o).extra.borrow() {
                    Extra::Closure(c) => Some(c.clone()),
                    _ => None,
                };
                if let Some(c) = cd {
                    if let Some(b) = &c.builtin {
                        return crate::harness::builtins::call(self, b, args);
                    }
                    if let Some((this, cls, m)) = &c.method {
                        return self.call_method_h(this.as_ref(), cls, m, args);
                    }
                    let st = c.this.as_ref().map(|t| tobj(t).class.clone()).or_else(|| c.scope.clone());
                    return self.call_user(&c.func, args, c.this.clone(), c.scope.clone(), st, &c.bound);
                }
                self.call_method(&o, "__invoke", args)
            }
            other => Err(rt_err(format!("Value not callable: {}", other.type_name()))),
        }
    }

    pub fn is_callable_value(&mut self, f: &Value) -> Result<(), String> {
        match f.deref() {
            Value::Str(name) => {
                let n = name.to_string_lossy();
                let n = n.trim_start_matches('\\').to_string();
                if let Some((c, m)) = n.split_once("::") {
                    return match self.find_class(c.as_bytes()) {
                        Some(cls) if cls.find_method(&m.to_ascii_lowercase()).is_some() => Ok(()),
                        Some(cls) => Err(format!("class {} does not have a method \"{m}\"", cls.name)),
                        None => Err(format!("class \"{c}\" not found")),
                    };
                }
                let l = n.to_ascii_lowercase();
                if self.functions.contains_key(&l) || crate::harness::builtins::exists(&l) {
                    Ok(())
                } else {
                    Err(format!("function \"{n}\" not found or invalid function name"))
                }
            }
            Value::Array(a) if a.len() == 2 => {
                let target = a.get_int(0).map(Value::deref).unwrap_or_default();
                let m = match a.get_int(1).map(Value::deref) {
                    Some(Value::Str(m)) => m.to_string_lossy(),
                    _ => return Err("second array member is not a valid method".into()),
                };
                let cls = match target {
                    Value::Object(o) => tobj(&o).class.clone(),
                    Value::Str(c) => match self.find_class(c.as_bytes()) {
                        Some(c) => c,
                        None => return Err(format!("class \"{}\" not found", c.to_string_lossy())),
                    },
                    _ => return Err("first array member is not a valid class name or object".into()),
                };
                let mm = m.strip_prefix("parent::").unwrap_or(&m);
                if cls.find_method(&mm.to_ascii_lowercase()).is_some() || cls.find_method("__call").is_some() {
                    Ok(())
                } else {
                    Err(format!("class {} does not have a method \"{m}\"", cls.name))
                }
            }
            Value::Array(_) => Err("array callback must have exactly two members".into()),
            Value::Object(o) => {
                if matches!(&*tobj(&o).extra.borrow(), Extra::Closure(_)) || tobj(&o).class.find_method("__invoke").is_some() {
                    Ok(())
                } else {
                    Err("no array or string given".into())
                }
            }
            _ => Err("no array or string given".into()),
        }
    }

    fn include_file(&mut self, p: &str, kind: IncludeKind) -> RtResult<Value> {
        let path = if p.starts_with('/') { p.to_string() } else { format!("{}/{}", self.dir, p) };
        let Ok(src) = std::fs::read(&path) else {
            let fname = match kind {
                IncludeKind::Require | IncludeKind::RequireOnce => "require",
                _ => "include",
            };
            self.emit(ErrorLevel::Warning, &format!("{fname}({p}): Failed to open stream: No such file or directory"))?;
            return Ok(Value::Bool(false));
        };
        let program = php_parser::parse(&src, &php_parser::ParseOptions::default())
            .map_err(|e| RuntimeError::Fatal(e.message.clone()))?;
        let stmts = Rc::new(program.statements);
        self.hoist(&stmts);
        match self.exec_block(&stmts)? {
            Flow::Return(v) => Ok(v),
            _ => Ok(Value::Int(1)),
        }
    }
}

pub fn is_lvalue(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Var(_) | ExprKind::Dim { .. } | ExprKind::Prop { .. } | ExprKind::StaticProp { .. })
}

pub fn cast_target_name(t: CastTarget) -> &'static str {
    t.type_name()
}
