//! Compiling and running files: the main script, `include`/`require`
//! and `eval`.

use crate::compiler::{self, CompileOptions, CompiledUnit};
use crate::opcode::IncludeKind;
use crate::vm::{Frame, FrameKind, RetTarget, Throw, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::rc::Rc;

/// Outcome of compiling source code.
pub(crate) enum CompileOutcome {
    Ok(Rc<CompiledUnit>),
    /// A `ParseError` exception was created.
    ParseError(php_runtime::Obj),
    /// A fatal error was reported.
    Fatal,
}

impl Vm {
    /// Parses and compiles source code.
    pub(crate) fn compile_source(&mut self, src: &[u8], filename: &ZStr, is_eval: bool, skip_shebang: bool) -> VmResult<CompileOutcome> {
        let parsed = if is_eval {
            php_parser::parse_eval(src)
        } else {
            let opts = php_parser::ParseOptions {
                short_open_tag: self.ini.get_bool("short_open_tag"),
                skip_shebang,
                ..Default::default()
            };
            php_parser::parse(src, &opts)
        };
        let program = match parsed {
            Ok(p) => p,
            Err(e) => {
                for w in &e.warnings {
                    let level = match w.level {
                        php_parser::ast::DiagnosticLevel::CompileWarning => 128,
                        php_parser::ast::DiagnosticLevel::Deprecated => 8192,
                        php_parser::ast::DiagnosticLevel::Warning => 2,
                    };
                    self.error_at(level, &w.message, filename, w.line)?;
                }
                return match e.kind {
                    php_parser::ErrorKind::Parse => {
                        let o = self.create_exception("ParseError", &e.message, 0)?;
                        self.set_exc_prop(&o, "file", Value::Str(filename.clone()));
                        self.set_exc_prop(&o, "line", Value::Int(i64::from(e.line)));
                        Ok(CompileOutcome::ParseError(o))
                    }
                    php_parser::ErrorKind::Compile => {
                        self.report_compile_error(&e.message, filename, e.line, false);
                        Ok(CompileOutcome::Fatal)
                    }
                };
            }
        };
        let opts = CompileOptions {
            filename: filename.clone(),
            is_eval,
            eval_class: None,
            assertions: self.ini.get_long("zend.assertions"),
        };
        let unit = match compiler::compile(&program, Rc::from(src), opts) {
            Ok(u) => u,
            Err(e) => {
                for w in &program.warnings {
                    let level = match w.level {
                        php_parser::ast::DiagnosticLevel::CompileWarning => 128,
                        php_parser::ast::DiagnosticLevel::Deprecated => 8192,
                        php_parser::ast::DiagnosticLevel::Warning => 2,
                    };
                    self.error_at(level, &w.message, filename, w.line)?;
                }
                self.report_compile_error(&e.message, filename, e.line, e.is_parse);
                return Ok(CompileOutcome::Fatal);
            }
        };
        if let Some(off) = program.halt_compiler_offset {
            self.ext
                .halt_offsets
                .insert(filename.clone(), i64::try_from(off).unwrap_or(0));
        }
        for (level, msg, line) in &unit.warnings {
            self.error_at(*level, msg, filename, *line)?;
        }
        let unit = Rc::new(unit);
        self.units.push(unit.clone());
        Ok(CompileOutcome::Ok(unit))
    }

    /// Declares the hoisted functions and classes of a unit.
    pub(crate) fn declare_hoisted(&mut self, unit: &CompiledUnit) -> VmResult<()> {
        for &i in &unit.hoisted_functions {
            let f = unit.decls.functions.borrow()[i as usize].clone();
            let lc = ZStr::from(f.name.to_ascii_lowercase());
            if let Some(existing) = self.functions.get(&lc) {
                let msg = match existing {
                    crate::func::Func::User(u) => format!(
                        "Cannot redeclare function {}() (previously declared in {}:{})",
                        f.name, u.code.filename, u.line_start
                    ),
                    crate::func::Func::Builtin(_) => format!("Cannot redeclare function {}()", f.name),
                };
                self.report_compile_error(&msg, &f.code.filename, f.line_start, false);
                return Err(Throw::Fatal);
            }
            self.functions.insert(lc, crate::func::Func::User(f));
        }
        for &i in &unit.hoisted_classes {
            let d = unit.decls.classes.borrow()[i as usize].clone();
            if self.classes.contains_key(&ZStr::from(d.name.to_ascii_lowercase())) {
                continue;
            }
            if self.can_early_bind(&d) {
                // Early binding; errors surface when the declaration runs.
                let saved = self.frames.len();
                let r = self.declare_class(&d, false);
                self.frames.truncate(saved.max(self.frames.len().min(saved)));
                if let Err(e) = r {
                    match e {
                        Throw::Fatal | Throw::Exit => return Err(e),
                        Throw::Exception(_) => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Maps a unit's CVs onto an existing variable store.
    fn cv_map_for(&mut self, code: &crate::opcode::OpArray, vars: &Rc<std::cell::RefCell<crate::vm::VarStore>>) -> Rc<[u32]> {
        let mut st = vars.borrow_mut();
        code.cv_names
            .iter()
            .map(|n| st.find_or_add(n.as_bytes()))
            .collect::<Vec<_>>()
            .into()
    }

    /// Runs the main script; returns normally after shutdown work is done
    /// by the caller.
    pub fn run_main(&mut self, src: &[u8], path: &ZStr) -> Result<(), Throw> {
        self.script_path = path.clone();
        let unit = match self.compile_source(src, path, false, true) {
            Ok(CompileOutcome::Ok(u)) => u,
            Ok(CompileOutcome::ParseError(e)) => {
                self.report_uncaught(&e);
                return Err(Throw::Fatal);
            }
            Ok(CompileOutcome::Fatal) => return Err(Throw::Fatal),
            Err(t) => return self.handle_top_throw(t),
        };
        if let Err(t) = self.declare_hoisted(&unit) {
            return self.handle_top_throw(t);
        }
        let globals = self.globals.clone();
        let map = self.cv_map_for(&unit.main.code, &globals);
        let mut f = Frame::with_vars(unit.main.code.clone(), FrameKind::Main, globals, Some(map));
        f.ufunc = None;
        self.frames.push(f);
        let base = self.frames.len() - 1;
        let r = self.execute(base);
        self.frames.truncate(base);
        match r {
            Ok(_) => Ok(()),
            Err(t) => self.handle_top_throw(t),
        }
    }

    /// Handles a throw escaping the top-level script.
    pub(crate) fn handle_top_throw(&mut self, t: Throw) -> Result<(), Throw> {
        match t {
            Throw::Exception(e) => self.uncaught_exception(e),
            other => Err(other),
        }
    }

    fn resolve_include_path(&self, name: &[u8]) -> Option<std::path::PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let p = std::path::Path::new(std::ffi::OsStr::from_bytes(name));
        if name.is_empty() {
            return None;
        }
        let is_relative_explicit = name.starts_with(b"./") || name.starts_with(b"../");
        if p.is_absolute() || is_relative_explicit {
            return p.is_file().then(|| p.to_path_buf());
        }
        let ip = self.ini.get("include_path").map(<[u8]>::to_vec).unwrap_or_else(|| b".".to_vec());
        for dir in ip.split(|&c| c == b':') {
            if dir.is_empty() {
                continue;
            }
            let d = std::path::Path::new(std::ffi::OsStr::from_bytes(dir));
            let cand = d.join(p);
            if cand.is_file() {
                return Some(cand);
            }
        }
        // The directory of the executing script.
        let (file, _) = self.current_location();
        let fb = file.as_bytes();
        if let Some(pos) = fb.iter().rposition(|&c| c == b'/') {
            let d = std::path::Path::new(std::ffi::OsStr::from_bytes(&fb[..pos]));
            let cand = d.join(p);
            if cand.is_file() {
                return Some(cand);
            }
        }
        None
    }

    /// `include`/`require`/`eval`. Returns `true` if a frame was pushed.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn op_include(&mut self, kind: IncludeKind, v: Value, dst: u32) -> VmResult<bool> {
        let s = self.to_zstr(&v)?;
        if kind == IncludeKind::Eval {
            let (file, line) = self.current_location();
            let fname = ZStr::from(format!("{}({}) : eval()'d code", file.to_string_lossy(), line));
            let unit = match self.compile_source(s.as_bytes(), &fname, true, false)? {
                CompileOutcome::Ok(u) => u,
                CompileOutcome::ParseError(e) => return Err(Throw::Exception(e)),
                CompileOutcome::Fatal => return Err(Throw::Fatal),
            };
            self.declare_hoisted(&unit)?;
            self.push_include_frame(&unit, FrameKind::Eval, dst, None);
            return Ok(true);
        }
        let kind_name = match kind {
            IncludeKind::Include => "include",
            IncludeKind::IncludeOnce => "include_once",
            IncludeKind::Require => "require",
            IncludeKind::RequireOnce => "require_once",
            IncludeKind::Eval => "eval",
        };
        let name = s.as_bytes();
        if name.contains(&0) {
            return Err(self.make_throw(
                "ValueError",
                &format!("{kind_name}(): Argument #1 ($filename) must not contain any null bytes"),
            ));
        }
        let resolved = self.resolve_include_path(name);
        let Some(path) = resolved else {
            let n = s.to_string_lossy();
            let ip = self
                .ini
                .get("include_path")
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .unwrap_or_default();
            if name.is_empty() {
                self.warning(&format!("{kind_name}(): Filename cannot be empty"))?;
            } else {
                self.warning(&format!("{kind_name}({n}): Failed to open stream: No such file or directory"))?;
            }
            if matches!(kind, IncludeKind::Require | IncludeKind::RequireOnce) {
                let msg = format!("Uncaught Error: Failed opening required '{n}' (include_path='{ip}')");
                let _ = msg;
                let (file, line) = self.current_location();
                self.report_compile_error(
                    &format!("{kind_name}(): Failed opening required '{n}' (include_path='{ip}')"),
                    &file,
                    line,
                    false,
                );
                return Err(Throw::Fatal);
            }
            self.warning(&format!(
                "{kind_name}(): Failed opening '{n}' for inclusion (include_path='{ip}')"
            ))?;
            self.set_tmp(dst, Value::Bool(false));
            return Ok(false);
        };
        let real = std::fs::canonicalize(&path).unwrap_or(path.clone());
        use std::os::unix::ffi::OsStrExt;
        let real_z = ZStr::from(real.as_os_str().as_bytes());
        if matches!(kind, IncludeKind::IncludeOnce | IncludeKind::RequireOnce) && self.included_set.contains(&real_z) {
            self.set_tmp(dst, Value::Bool(true));
            return Ok(false);
        }
        let src = match std::fs::read(&path) {
            Ok(s) => s,
            Err(e) => {
                let n = s.to_string_lossy();
                self.warning(&format!("{kind_name}({n}): Failed to open stream: {e}"))?;
                self.set_tmp(dst, Value::Bool(false));
                return Ok(false);
            }
        };
        let display_path = if path.is_absolute() {
            ZStr::from(path.as_os_str().as_bytes())
        } else {
            real_z.clone()
        };
        if !self.included_set.contains(&real_z) {
            self.included_set.insert(real_z.clone());
            self.included.push(real_z);
        }
        let unit = match self.compile_source(&src, &display_path, false, false)? {
            CompileOutcome::Ok(u) => u,
            CompileOutcome::ParseError(e) => return Err(Throw::Exception(e)),
            CompileOutcome::Fatal => return Err(Throw::Fatal),
        };
        self.declare_hoisted(&unit)?;
        self.push_include_frame(&unit, FrameKind::Include, dst, Some(kind_name));
        Ok(true)
    }

    fn push_include_frame(&mut self, unit: &Rc<CompiledUnit>, kind: FrameKind, dst: u32, kind_name: Option<&str>) {
        let vars = self.frame().vars.clone();
        let map = self.cv_map_for(&unit.main.code, &vars);
        let mut f = Frame::with_vars(unit.main.code.clone(), kind, vars, Some(map));
        let cur = self.frame();
        f.this = cur.this.clone();
        f.scope = cur.scope.clone();
        f.called = cur.called.clone();
        f.ret = RetTarget::Tmp(dst);
        f.builtin_name = kind_name.map(ZStr::from);
        self.frames.push(f);
    }

    /// Runs a file as `include` would from builtin code (e.g. the
    /// `auto_prepend_file`).
    pub fn include_file(&mut self, path: &str) -> Result<(), Throw> {
        let src = match std::fs::read(path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };
        let p = ZStr::from(path);
        let unit = match self.compile_source(&src, &p, false, false)? {
            CompileOutcome::Ok(u) => u,
            CompileOutcome::ParseError(e) => return self.handle_top_throw(Throw::Exception(e)),
            CompileOutcome::Fatal => return Err(Throw::Fatal),
        };
        self.declare_hoisted(&unit)?;
        let globals = self.globals.clone();
        let map = self.cv_map_for(&unit.main.code, &globals);
        let f = Frame::with_vars(unit.main.code.clone(), FrameKind::Main, globals, Some(map));
        self.frames.push(f);
        let base = self.frames.len() - 1;
        let r = self.execute(base);
        self.frames.truncate(base);
        match r {
            Ok(_) => Ok(()),
            Err(t) => self.handle_top_throw(t),
        }
    }
}
