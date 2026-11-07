//! Compiling and running files: the main script, `include`/`require`
//! and `eval`.

use crate::compiler::{self, CompileOptions, CompiledUnit, CtConstants};
use crate::opcode::IncludeKind;
use crate::vm::{Frame, FrameKind, RetTarget, Throw, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::rc::Rc;

/// Outcome of compiling source code.
#[derive(Clone)]
pub enum CompileOutcome {
    Ok(Rc<CompiledUnit>),
    /// A `ParseError` (or thrown `CompileError`) exception was created.
    ParseError(php_runtime::Obj),
    /// A fatal error was reported.
    Fatal,
}

/// A hook called when compiling file source (`zend_compile_file` override point).
pub type CompileHookFn = fn(&mut Vm, Rc<[u8]>, &ZStr, bool) -> VmResult<CompileOutcome>;

thread_local! {
    static COMPILE_HOOK: std::cell::Cell<Option<CompileHookFn>> = const { std::cell::Cell::new(None) };
}

impl Vm {
    /// Installs a compile hook (`zend_compile_file` override point) for files.
    pub fn set_compile_hook(f: CompileHookFn) {
        COMPILE_HOOK.with(|h| h.set(Some(f)));
    }

    /// The constants the compiler may substitute (`zend_try_ct_eval_const`,
    /// `can_ct_eval_const` in `Zend/zend_compile.c`): the internal (not
    /// user-defined) constants with a scalar value that are not deprecated.
    /// Built once and rebuilt only when internal or deprecated constants
    /// are added or removed.
    pub(crate) fn ct_constants(&mut self) -> CtConstants {
        let key = (
            self.constants
                .len()
                .saturating_sub(self.user_constants.len()),
            self.deprecated_constants.len(),
        );
        if let Some((k, c)) = &self.ct_constants {
            if *k == key {
                return c.clone();
            }
        }
        // Sized for every constant so the table never rehashes while it
        // fills (2 700 internal constants rehashed eleven times before), and
        // the few user-defined and deprecated names are removed afterwards
        // instead of being looked up once per constant.
        let mut table: std::collections::HashMap<ZStr, Value> =
            std::collections::HashMap::with_capacity(self.constants.len());
        table.extend(
            self.constants
                .iter()
                .filter(|(_, v)| {
                    matches!(
                        v,
                        Value::Null
                            | Value::Bool(_)
                            | Value::Int(_)
                            | Value::Float(_)
                            | Value::Str(_)
                    )
                })
                .map(|(name, v)| (name.clone(), v.clone())),
        );
        for name in &self.user_constants {
            table.remove(name);
        }
        for name in self.deprecated_constants.keys() {
            table.remove(name);
        }
        let c = CtConstants::new(table);
        self.ct_constants = Some((key, c.clone()));
        c
    }

    /// Parses source with current INI and mbstring request settings.
    ///
    /// For eval, Zend scans in the effective internal encoding and bypasses
    /// Unicode detection. Files use `zend.script_encoding` and BOM detection.
    pub fn parse_source(
        &self,
        src: &[u8],
        is_eval: bool,
        skip_shebang: bool,
    ) -> Result<php_parser::Program, php_parser::ParseError> {
        let opts = php_parser::ParseOptions {
            short_open_tag: self.ini.get_bool("short_open_tag"),
            start_in_scripting: is_eval,
            skip_shebang,
            multibyte: self.ini.get_bool("zend.multibyte")
                && self.script_encoding_provider.is_some(),
            ..Default::default()
        };
        let Some(provider) = self.script_encoding_provider.filter(|_| opts.multibyte) else {
            return php_parser::parse(src, &opts);
        };
        let current = provider(self);
        let script_encodings: Vec<_> = if is_eval {
            current.options.internal_encoding.into_iter().collect()
        } else {
            current.script_encodings
        };
        let encoding = php_parser::ScriptEncodingOptions {
            script_encodings: &script_encodings,
            detect_unicode: !is_eval && self.ini.get_bool("zend.detect_unicode"),
            precision: usize::try_from(self.ini.get_long("precision")).unwrap_or(14),
            ..current.options
        };
        php_parser::parse_with_encoding(src, &opts, &encoding)
    }

    /// Parses and compiles source code, calling the compile hook for files.
    pub(crate) fn compile_source(
        &mut self,
        src: Rc<[u8]>,
        filename: &ZStr,
        is_eval: bool,
        skip_shebang: bool,
    ) -> VmResult<CompileOutcome> {
        if !is_eval {
            if let Some(hook) = COMPILE_HOOK.with(|h| h.get()) {
                return hook(self, src, filename, skip_shebang);
            }
        }
        self.compile_source_default(src, filename, is_eval, skip_shebang)
    }

    /// Parses and compiles source code with the default compiler pipeline.
    ///
    /// The source comes as the `Rc` the compiled unit keeps, so the caller's
    /// buffer is released before parsing and a large include is not held
    /// twice while its AST is alive.
    pub fn compile_source_default(
        &mut self,
        src: Rc<[u8]>,
        filename: &ZStr,
        is_eval: bool,
        skip_shebang: bool,
    ) -> VmResult<CompileOutcome> {
        let parsed = self.parse_source(&src, is_eval, skip_shebang);
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
                    // `zend_throw_exception(zend_ce_compile_error, ...)`
                    // during parsing (bug75218).
                    php_parser::ErrorKind::Compile if e.thrown => {
                        let o = self.create_exception("CompileError", &e.message, 0)?;
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
            eval_parent: None,
            closure_name: None,
            strict_types: false,
            sprintf_builtin: matches!(
                self.functions.get(&ZStr::from("sprintf")),
                Some(crate::func::Func::Builtin(_))
            ),
            array_map_builtin: matches!(
                self.functions.get(&ZStr::from("array_map")),
                Some(crate::func::Func::Builtin(_))
            ),
            strlen_builtin: matches!(
                self.functions.get(&ZStr::from("strlen")),
                Some(crate::func::Func::Builtin(_))
            ),
            special_builtins: self.special_builtins(),
            anon_base: self.anon_counter,
            ct_constants: Some(self.ct_constants()),
            stack_limit: self.compile_stack_limit(),
            user_opcodes: self.user_opcodes(),
            optimization_level: if self.ini.get_bool("opcache.enable")
                && (self.ini.get_bool("opcache.enable_cli")
                    || self.ini.get_bool("opcache.file_cache_only"))
            {
                u32::try_from(self.ini.get_long("opcache.optimization_level")).unwrap_or(0)
            } else {
                0
            },
        };
        let halt_compiler_offset = program.halt_compiler_offset;
        // A multibyte script's AST is kept for the semantic text of
        // diagnostics; any other is released statement by statement while
        // it compiles. The compiler sees the function table as it stands
        // (`CG(function_table)`), for the `fbc` of calls by name.
        let functions = &self.functions;
        let table = |lc: &[u8]| functions.get(lc).map(compiler::FunctionInfo::of);
        let (compiled, kept) = if program.multibyte {
            let unit = compiler::compile_with_functions(&program, src, opts, &table);
            (unit, Some(program))
        } else {
            let unit = compiler::compile_program_with_functions(program, src, opts, &table);
            (unit, None)
        };
        self.activate_compiled_auto_globals();
        let unit = match compiled {
            Ok(u) => {
                self.anon_counter = u.anon_end;
                u
            }
            Err(e) => {
                for (level, msg, line) in &e.warnings {
                    self.error_at(*level, msg, filename, *line)?;
                }
                self.report_compile_error(&e.message, filename, e.line, e.is_parse);
                return Ok(CompileOutcome::Fatal);
            }
        };
        if let Some(off) = halt_compiler_offset {
            self.ext
                .halt_offsets
                .insert(filename.clone(), i64::try_from(off).unwrap_or(0));
        }
        for (level, msg, line) in &unit.warnings {
            self.error_at(*level, msg, filename, *line)?;
        }
        if let Some(program) = kept {
            self.parsed_sources
                .borrow_mut()
                .insert(filename.clone(), Rc::new(program));
        }
        let unit = Rc::new(unit);
        if declares_anything(&unit) {
            self.units.push(unit.clone());
        }
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
                    crate::func::Func::Builtin(_) => {
                        format!("Cannot redeclare function {}()", f.name)
                    }
                };
                self.report_compile_error(&msg, &f.code.filename, f.line_start, false);
                return Err(Throw::Fatal);
            }
            self.observer_declared(&lc, false);
            self.functions.insert(lc, crate::func::Func::User(f));
        }
        for &i in &unit.hoisted_classes {
            let d = unit.decls.classes.borrow()[i as usize].clone();
            if self
                .classes
                .contains_key(&ZStr::from(d.name.to_ascii_lowercase()))
            {
                continue;
            }
            if self.can_early_bind(&d) {
                // Early binding; errors surface when the declaration runs.
                let saved = self.frames.len();
                let r = self.declare_class(&d, false);
                self.frames
                    .truncate(saved.max(self.frames.len().min(saved)));
                if let Err(e) = r {
                    match e {
                        Throw::Fatal | Throw::Exit | Throw::FiberSuspend => return Err(e),
                        Throw::Exception(_) => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// php-src's C stack at the start of a compilation, for the compiler's
    /// `zend_check_stack_limit()` model: the usable stack
    /// (`zend.max_allowed_stack_size - zend.reserved_stack_size`, whose 0
    /// default reserves `ZEND_ALLOCA_MAX_SIZE + 16 KiB`; with an automatic
    /// maximum, the one the runtime checks use) and the bytes in use (the
    /// native stack used since the first execution, scaled to php-src's
    /// frames as `native_stack_exhausted` does, plus php-src's frames from
    /// `main()` down to the first `zend_compile_expr()`).
    #[cold]
    fn compile_stack_limit(&self) -> Option<compiler::CompileStackLimit> {
        const DEFAULT_RESERVED: usize = 48 << 10;
        const COMPILE_BASE_BYTES: usize = 24 << 10;
        /// `zend_call_stack_init()` when the stack cannot be detected:
        /// `zend_call_stack_default_size()` (8 MiB on Linux) less the
        /// 32 KiB it takes off because the base is not the real one.
        const UNDETECTED_STACK_BYTES: usize = (8 << 20) - (32 << 10);
        let reserved = match usize::try_from(self.ini.get_long("zend.reserved_stack_size")) {
            Ok(0) | Err(_) => DEFAULT_RESERVED,
            Ok(r) => r,
        };
        let stack = match usize::try_from(self.ini.get_long("zend.max_allowed_stack_size")) {
            // -1: unchecked.
            Err(_) => return None,
            // 0: detect; a thread of unknown stack size (declared 0) gets
            // php-src's fallback instead of an empty budget.
            Ok(0) if self.native_stack_size == 0 => UNDETECTED_STACK_BYTES,
            Ok(0) => self.native_stack_size / crate::fiber::STACK_SCALE,
            Ok(m) => m,
        };
        let marker = 0u8;
        let here = std::ptr::addr_of!(marker) as usize;
        let running = self.native_stack_base.saturating_sub(here) / crate::fiber::STACK_SCALE;
        Some(compiler::CompileStackLimit {
            used: COMPILE_BASE_BYTES.saturating_add(running),
            size: stack.saturating_sub(reserved),
        })
    }

    /// Runs the main script; returns normally after shutdown work is done
    /// by the caller.
    pub fn run_main(&mut self, src: &[u8], path: &ZStr) -> Result<(), Throw> {
        self.run_top_level(src, path, false)
    }

    /// Models `zend_call_stack_overflowed()` in `zend_hash_compare` as a
    /// nesting limit for array comparisons: the usable stack
    /// (`zend.max_allowed_stack_size` minus `zend.reserved_stack_size`,
    /// whose 0 default is 48 KiB) over the C stack one nesting level of
    /// `zend_compare` takes.
    #[cold]
    fn apply_compare_depth_limit(&mut self) {
        /// Approximate C stack bytes per nesting level of `zend_compare` /
        /// `zend_hash_compare_impl` / `hash_zval_compare_function`.
        const COMPARE_FRAME_BYTES: usize = 256;
        let max = self.ini.get_long("zend.max_allowed_stack_size");
        let reserved = match usize::try_from(self.ini.get_long("zend.reserved_stack_size")) {
            Ok(0) | Err(_) => 48 << 10,
            Ok(r) => r,
        };
        let limit = match usize::try_from(max) {
            Err(_) => usize::MAX,
            Ok(0) => self.native_stack_size.saturating_sub(reserved) / COMPARE_FRAME_BYTES,
            Ok(m) => m.saturating_sub(reserved) / COMPARE_FRAME_BYTES,
        };
        php_runtime::ops::set_compare_depth_limit(limit.max(1));
    }

    /// Runs top-level eval input, as CLI `-r`, `-B`, `-R`, and `-E` do.
    /// Input has no opening tag and uses Zend's configured internal encoding.
    pub fn run_eval(&mut self, src: &[u8], path: &ZStr) -> Result<(), Throw> {
        self.run_top_level(src, path, true)
    }

    fn run_top_level(&mut self, src: &[u8], path: &ZStr, is_eval: bool) -> Result<(), Throw> {
        self.script_path = path.clone();
        self.apply_compare_depth_limit();
        if !is_eval {
            // php_execute_script adds the primary script to
            // EG(included_files): `require_once` of it is a no-op (bug60825).
            use std::os::unix::ffi::OsStrExt;
            if let Ok(real) = std::fs::canonicalize(std::ffi::OsStr::from_bytes(path.as_bytes())) {
                self.included_set
                    .insert(ZStr::from(real.as_os_str().as_bytes()));
            }
        }
        let src = if is_eval {
            src.to_vec()
        } else {
            match self.filter_source(path, src.to_vec()) {
                Ok(s) => s,
                Err(t) => return self.handle_top_throw(t),
            }
        };
        let unit = match self.compile_source(Rc::from(src), path, is_eval, !is_eval) {
            Ok(CompileOutcome::Ok(u)) => u,
            Ok(CompileOutcome::ParseError(e)) => {
                self.report_uncaught(&e);
                return Err(Throw::Fatal);
            }
            Ok(CompileOutcome::Fatal) => return Err(Throw::Fatal),
            Err(t) => return self.handle_top_throw(t),
        };
        let globals = self.globals.clone();
        let mut f = Frame::with_vars(unit.main.code.clone(), FrameKind::Main, globals, None);
        f.ufunc = None;
        self.frames.push(f);
        let base = self.frames.len() - 1;
        if let Err(t) = self.declare_hoisted(&unit) {
            self.frames.truncate(base);
            return self.handle_top_throw(t);
        }
        self.observer_begin_top();
        let r = self.execute(base);
        if let Some(f) = self.frames.get_mut(base) {
            f.attach_symbol_table();
        }
        self.frames.truncate(base);
        match r {
            Ok(v) => {
                self.main_return = v;
                Ok(())
            }
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
        let ip = self
            .ini
            .get("include_path")
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| b".".to_vec());
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
            let fname = ZStr::from(format!(
                "{}({}) : eval()'d code",
                file.to_string_lossy(),
                line
            ));
            let unit = match self.compile_source(Rc::from(s.as_bytes()), &fname, true, false)? {
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
        // Extension opener (stream wrappers, include_path entries such as
        // `phar://`): `php_stream_open_for_zend_ex`.
        if let Some(opener) = self.include_opener {
            match opener(self, name, kind_name)? {
                crate::ext::IncludeOpen::Opened {
                    source,
                    opened_path,
                } => {
                    return self.include_source(kind, kind_name, source, opened_path, dst);
                }
                crate::ext::IncludeOpen::Failed => {
                    return self.include_failed(kind, kind_name, &s, dst);
                }
                crate::ext::IncludeOpen::NotHandled => {}
            }
        }
        let resolved = self.resolve_include_path(name);
        let Some(path) = resolved else {
            if name.is_empty() {
                self.warning(&format!("{kind_name}(): Filename cannot be empty"))?;
            } else {
                self.include_warning(
                    kind_name,
                    name,
                    "Failed to open stream: No such file or directory",
                )?;
            }
            return self.include_failed(kind, kind_name, &s, dst);
        };
        use std::os::unix::ffi::OsStrExt;
        let real_z = ZStr::from(opened_path(&path).as_os_str().as_bytes());
        if matches!(kind, IncludeKind::IncludeOnce | IncludeKind::RequireOnce)
            && self.included_set.contains(&real_z)
        {
            self.set_tmp(dst, Value::Bool(true));
            return Ok(false);
        }
        let src = match std::fs::read(&path) {
            Ok(s) => s,
            Err(e) => {
                // strerror() text, without Rust's " (os error N)" suffix.
                let text = e.to_string();
                let text = text.split(" (os error").next().unwrap_or_default();
                self.include_warning(kind_name, name, &format!("Failed to open stream: {text}"))?;
                return self.include_failed(kind, kind_name, &s, dst);
            }
        };
        // The unit is compiled under the opened path, as `open_file_for_scanning`
        // uses `file_handle->opened_path` when there is one: `__FILE__`,
        // `__DIR__`, reflection's `getFileName()`, backtraces and messages
        // all show the resolved path, not the `..`/symlink spelling used.
        if !self.included_set.contains(&real_z) {
            self.included_set.insert(real_z.clone());
            self.included.push(real_z.clone());
        }
        let src = self.filter_source(&real_z, src)?;
        let unit = match self.compile_source(Rc::from(src), &real_z, false, true)? {
            CompileOutcome::Ok(u) => u,
            CompileOutcome::ParseError(e) => return Err(Throw::Exception(e)),
            CompileOutcome::Fatal => return Err(Throw::Fatal),
        };
        self.declare_hoisted(&unit)?;
        self.push_include_frame(&unit, FrameKind::Include, dst, Some(kind_name));
        Ok(true)
    }

    /// Includes already-opened source (from an opener hook).
    fn include_source(
        &mut self,
        kind: IncludeKind,
        kind_name: &str,
        src: Vec<u8>,
        path: ZStr,
        dst: u32,
    ) -> VmResult<bool> {
        if matches!(kind, IncludeKind::IncludeOnce | IncludeKind::RequireOnce)
            && self.included_set.contains(&path)
        {
            self.set_tmp(dst, Value::Bool(true));
            return Ok(false);
        }
        if !self.included_set.contains(&path) {
            self.included_set.insert(path.clone());
            self.included.push(path.clone());
        }
        let src = self.filter_source(&path, src)?;
        let unit = match self.compile_source(Rc::from(src), &path, false, true)? {
            CompileOutcome::Ok(u) => u,
            CompileOutcome::ParseError(e) => return Err(Throw::Exception(e)),
            CompileOutcome::Fatal => return Err(Throw::Fatal),
        };
        self.declare_hoisted(&unit)?;
        self.push_include_frame(&unit, FrameKind::Include, dst, Some(kind_name));
        Ok(true)
    }

    /// The "Failed opening" tail of a failed include (the opener already
    /// reported why).
    fn include_failed(
        &mut self,
        kind: IncludeKind,
        kind_name: &str,
        s: &ZStr,
        dst: u32,
    ) -> VmResult<bool> {
        let n = s.to_string_lossy();
        let ip = self
            .ini
            .get("include_path")
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();
        // `php_message_handler_for_zend`: ZMSG_FAILED_REQUIRE_FOPEN throws.
        if matches!(kind, IncludeKind::Require | IncludeKind::RequireOnce) {
            return Err(self.make_throw(
                "Error",
                &format!("Failed opening required '{n}' (include_path='{ip}')"),
            ));
        }
        self.include_warning(
            kind_name,
            s.as_bytes(),
            &format!("Failed opening '{n}' for inclusion (include_path='{ip}')"),
        )?;
        self.set_tmp(dst, Value::Bool(false));
        Ok(false)
    }

    /// Runs the source-filter hook (e.g. phar stubs) over script source.
    pub(crate) fn filter_source(&mut self, path: &ZStr, src: Vec<u8>) -> VmResult<Vec<u8>> {
        match self.source_filter {
            Some(f) => f(self, path, src),
            None => Ok(src),
        }
    }

    fn push_include_frame(
        &mut self,
        unit: &Rc<CompiledUnit>,
        kind: FrameKind,
        dst: u32,
        kind_name: Option<&str>,
    ) {
        self.frame_mut().attach_symbol_table();
        self.frame_mut().sync_cvs_to_symtab();
        let vars = self.frame().vars.clone();
        let mut f = Frame::with_vars(unit.main.code.clone(), kind, vars, None);
        let cur = self.frame();
        f.this = cur.this.clone();
        f.scope = cur.scope.clone();
        f.called = cur.called.clone();
        f.ret = RetTarget::Tmp(dst);
        f.builtin_name = kind_name.map(ZStr::from);
        self.frames.push(f);
        self.observer_begin_top();
    }

    /// Runs a file as `include` would from builtin code (e.g. the
    /// `auto_prepend_file`, `spl_autoload()`), compiled under its opened
    /// path like an included file.
    pub fn include_file(&mut self, path: &str) -> Result<(), Throw> {
        use std::os::unix::ffi::OsStrExt;
        let src = match std::fs::read(path) {
            Ok(s) => s,
            Err(_) => return Ok(()),
        };
        let p = ZStr::from(
            opened_path(std::path::Path::new(path))
                .as_os_str()
                .as_bytes(),
        );
        let unit = match self.compile_source(Rc::from(src), &p, false, true)? {
            CompileOutcome::Ok(u) => u,
            CompileOutcome::ParseError(e) => return self.handle_top_throw(Throw::Exception(e)),
            CompileOutcome::Fatal => return Err(Throw::Fatal),
        };
        self.declare_hoisted(&unit)?;
        let globals = self.globals.clone();
        let f = Frame::with_vars(unit.main.code.clone(), FrameKind::Main, globals, None);
        self.frames.push(f);
        self.observer_begin_top();
        let base = self.frames.len() - 1;
        let r = self.execute(base);
        if let Some(f) = self.frames.get_mut(base) {
            f.attach_symbol_table();
        }
        self.frames.truncate(base);
        match r {
            Ok(_) => Ok(()),
            Err(t) => self.handle_top_throw(t),
        }
    }
}

/// The `opened_path` of a plain file found at `path` for inclusion: its
/// real path, as `php_resolve_path()` (`tsrm_realpath`) resolves it before
/// `_php_stream_fopen` opens it with `STREAM_ASSUME_REALPATH`, so `..`
/// segments and symbolic links are resolved. If that fails the path is
/// only made absolute (`expand_filepath`).
fn opened_path(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Whether a compiled unit declares functions, closures, classes or
/// attributed constants: only such units stay registered in
/// [`Vm::units`] (Reflection looks closures up there and
/// preloading images them). A unit that declares nothing is released once
/// its code has run and nothing else holds it, like the op array php-src's
/// `zend_include_or_eval` / `zend_eval_stringl` destroys after executing an
/// included file or `eval()`'d string; keeping every such unit made each
/// `eval()` call retain its op array for the rest of the request.
fn declares_anything(unit: &CompiledUnit) -> bool {
    !unit.hoisted_functions.is_empty()
        || !unit.hoisted_classes.is_empty()
        || !unit.decls.functions.borrow().is_empty()
        || !unit.decls.classes.borrow().is_empty()
        || !unit.decls.const_attributes.borrow().is_empty()
}

impl CompiledUnit {
    /// Clears run-time caches on all op arrays in this unit so the unit
    /// carries no request state.
    pub fn clear_runtime_caches(&self) {
        self.main.code.clear_runtime_caches();
        self.main.statics.borrow_mut().clear();
        for f in self.decls.functions.borrow().iter() {
            f.code.clear_runtime_caches();
            f.statics.borrow_mut().clear();
        }
        for c in self.decls.classes.borrow().iter() {
            for m in &c.methods {
                m.func.code.clear_runtime_caches();
                m.func.statics.borrow_mut().clear();
            }
        }
    }
}
