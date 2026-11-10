//! Declaration promotion across a startup/request boundary. Only compiled
//! declarations are retained; executor values and linked class caches are not.
//!
//! ref: ext/opcache/ZendAccelerator.c (`accel_preload`, `preload_link`,
//! `preload_remove_declares`)

use crate::class::{cflags, AttributeInfo, ClassDecl, ConstExpr, PropHooks};
use crate::compiler::CompiledUnit;
use crate::func::{Func, UserFunction};
use crate::opcode::{DeclTable, Op, OpArray, ParamInfo};
use crate::{Throw, Vm, VmResult};
use php_runtime::{Value, ZStr};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// What preloading made persistent, as `opcache_get_status()` reports it
/// (`ZCSG(preload_script)` and `ZCSG(saved_scripts)`).
#[derive(Debug, Clone, Default)]
pub struct PreloadInfo {
    /// The preloaded functions, by declared name.
    pub functions: Vec<ZStr>,
    /// The preloaded classes, by declared name.
    pub classes: Vec<ZStr>,
    /// The scripts compiled while preloading (the preload script first).
    pub scripts: Vec<ZStr>,
}

/// The preloaded declarations of the running request, by lowercase name.
struct Preloaded {
    info: Rc<PreloadInfo>,
    functions: HashSet<ZStr>,
    classes: HashSet<ZStr>,
}

thread_local! {
    /// `ZCG(preloading)`: the preload script is running (until the startup
    /// request has shut down and its declarations are linked).
    static PRELOADING: Cell<bool> = const { Cell::new(false) };
    /// The declarations installed from the preload image.
    static PRELOADED: RefCell<Option<Preloaded>> = const { RefCell::new(None) };
}

/// Indexes (into the unit's declaration table) of the class declarations
/// in the unit's top-level statement list (`ZEND_ACC_TOP_LEVEL`): the
/// `DeclareClass` ops of the main op array that no jump crosses. A
/// declaration inside an `if`, a loop, a `switch` or a `try` is crossed
/// by the jump around its block (or back over it).
fn top_level_classes(code: &OpArray) -> Vec<u32> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (at, op) in code.ops.iter().enumerate() {
        let mut op = op.clone();
        crate::compiler::optimizer::for_each_label_mut(&mut op, |t| {
            let target = *t as usize;
            spans.push((at.min(target), at.max(target)));
        });
    }
    code.ops
        .iter()
        .enumerate()
        .filter_map(|(at, op)| match op {
            Op::DeclareClass(idx) if !spans.iter().any(|&(lo, hi)| lo < at && at < hi) => {
                Some(*idx)
            }
            _ => None,
        })
        .collect()
}

/// The name of a class as `%s` prints it: an anonymous class's name ends
/// at its NUL byte.
fn printable_class_name(name: &ZStr) -> String {
    let bytes = name.as_bytes();
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Whether `existing` (a declared class) is the one `decl` declares: the
/// same name, file and line.
fn same_class_declaration(existing: &crate::class::Class, decl: &ClassDecl) -> bool {
    existing.line_start == decl.line_start
        && existing
            .filename
            .as_ref()
            .is_some_and(|f| f.as_bytes() == decl.filename.as_bytes())
}

/// Immutable declaration image produced by a completed preload request.
/// Installation makes an independent graph for every subsequent request.
pub struct PersistentDeclarations {
    functions: Vec<(ZStr, Rc<UserFunction>)>,
    classes: Vec<Rc<ClassDecl>>,
    aliases: Vec<(ZStr, ZStr)>,
    units: Vec<Rc<CompiledUnit>>,
    halt_offsets: HashMap<ZStr, i64>,
    parsed_sources: HashMap<ZStr, Rc<php_parser::Program>>,
    info: Rc<PreloadInfo>,
}

impl std::fmt::Debug for PersistentDeclarations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentDeclarations")
            .field("functions", &self.functions.len())
            .field("classes", &self.classes.len())
            .field("aliases", &self.aliases.len())
            .field("units", &self.units.len())
            .finish_non_exhaustive()
    }
}

/// Clone the compiled graph, preserving shared declaration-table identity.
/// Tables are populated after their users so recursive functions cannot
/// recursively clone their own declaration table.
#[derive(Default)]
struct ImageBuilder {
    functions: HashMap<usize, Rc<UserFunction>>,
    code: HashMap<usize, Rc<OpArray>>,
    classes: HashMap<usize, Rc<ClassDecl>>,
    tables: HashMap<usize, Rc<DeclTable>>,
    pending: Vec<(Rc<DeclTable>, Rc<DeclTable>)>,
}

impl ImageBuilder {
    fn table(&mut self, original: &Rc<DeclTable>) -> Rc<DeclTable> {
        let key = Rc::as_ptr(original) as usize;
        if let Some(table) = self.tables.get(&key) {
            return table.clone();
        }
        let table = Rc::new(DeclTable::default());
        self.tables.insert(key, table.clone());
        self.pending.push((original.clone(), table.clone()));
        table
    }

    fn expr(&mut self, expr: &ConstExpr) -> ConstExpr {
        match expr {
            ConstExpr::Value(v) => ConstExpr::Value(v.clone()),
            ConstExpr::Code(code) => ConstExpr::Code(self.code(code)),
        }
    }

    fn attributes(&mut self, attrs: &[AttributeInfo]) -> Vec<AttributeInfo> {
        attrs
            .iter()
            .map(|attr| AttributeInfo {
                name: attr.name.clone(),
                args: attr
                    .args
                    .iter()
                    .map(|(n, e)| (n.clone(), self.expr(e)))
                    .collect(),
                line: attr.line,
                strict: attr.strict,
            })
            .collect()
    }

    fn params(&mut self, params: &[ParamInfo]) -> Vec<ParamInfo> {
        params
            .iter()
            .map(|p| {
                let mut p = p.clone();
                p.default_expr = p.default_expr.as_ref().map(|e| self.expr(e));
                p.attributes = self.attributes(&p.attributes);
                p
            })
            .collect()
    }

    fn code(&mut self, original: &Rc<OpArray>) -> Rc<OpArray> {
        let key = Rc::as_ptr(original) as usize;
        if let Some(code) = self.code.get(&key) {
            return code.clone();
        }
        let code = Rc::new(OpArray {
            ops: original.ops.clone(),
            lines: original.lines.clone(),
            literals: original.literals.clone(),
            places: original.places.clone(),
            cv_names: original.cv_names.clone(),
            num_tmps: original.num_tmps,
            num_iters: original.num_iters,
            num_finally: original.num_finally,
            num_statics: original.num_statics,
            try_table: original.try_table.clone(),
            live_tmps: original.live_tmps.clone(),
            filename: original.filename.clone(),
            strict_types: original.strict_types,
            decls: self.table(&original.decls),
            fcall_cache: RefCell::new(Vec::new()),
            class_cache: RefCell::new(Vec::new()),
            rt_cache: crate::rtcache::RuntimeCache::default(),
            prop_cache: RefCell::new(Vec::new()),
            method_cache: RefCell::new(Vec::new()),
        });
        self.code.insert(key, code.clone());
        code
    }

    fn function(&mut self, original: &Rc<UserFunction>) -> Rc<UserFunction> {
        let key = Rc::as_ptr(original) as usize;
        if let Some(function) = self.functions.get(&key) {
            return function.clone();
        }
        let function = Rc::new(UserFunction {
            name: original.name.clone(),
            params: self.params(&original.params),
            required: original.required,
            return_type: original.return_type.clone(),
            flags: original.flags,
            code: self.code(&original.code),
            line_start: original.line_start,
            line_end: original.line_end,
            doc_comment: original.doc_comment.clone(),
            statics: RefCell::new(Vec::new()),
            uses: original.uses.clone(),
            class_name: original.class_name.clone(),
            attributes: self.attributes(&original.attributes),
            hook_prop: original.hook_prop.clone(),
        });
        self.functions.insert(key, function.clone());
        function
    }

    fn class(&mut self, original: &Rc<ClassDecl>) -> Rc<ClassDecl> {
        let key = Rc::as_ptr(original) as usize;
        if let Some(class) = self.classes.get(&key) {
            return class.clone();
        }
        let mut class = (**original).clone();
        class.attributes = self.attributes(&class.attributes);
        for constant in &mut class.consts {
            constant.value = self.expr(&constant.value);
            constant.attributes = self.attributes(&constant.attributes);
        }
        for property in &mut class.props {
            property.default = property.default.as_ref().map(|e| self.expr(e));
            property.attributes = self.attributes(&property.attributes);
            property.hooks = property.hooks.as_ref().map(|h| {
                Rc::new(PropHooks {
                    get: h.get.as_ref().map(|f| self.function(f)),
                    set: h.set.as_ref().map(|f| self.function(f)),
                    ..(**h).clone()
                })
            });
        }
        for method in &mut class.methods {
            method.func = self.function(&method.func);
        }
        for (_, value, _) in &mut class.cases {
            *value = value.as_ref().map(|e| self.expr(e));
        }
        class.case_attributes = class
            .case_attributes
            .iter()
            .map(|a| self.attributes(a))
            .collect();
        let class = Rc::new(class);
        self.classes.insert(key, class.clone());
        class
    }

    fn unit(&mut self, original: &CompiledUnit) -> Rc<CompiledUnit> {
        Rc::new(CompiledUnit {
            main: self.function(&original.main),
            hoisted_functions: original.hoisted_functions.clone(),
            hoisted_classes: original.hoisted_classes.clone(),
            decls: self.table(&original.decls),
            warnings: original.warnings.clone(),
            anon_end: original.anon_end,
        })
    }

    fn finish(&mut self) {
        while let Some((original, table)) = self.pending.pop() {
            *table.functions.borrow_mut() = original
                .functions
                .borrow()
                .iter()
                .map(|f| self.function(f))
                .collect();
            *table.classes.borrow_mut() = original
                .classes
                .borrow()
                .iter()
                .map(|c| self.class(c))
                .collect();
            *table.const_attributes.borrow_mut() = original
                .const_attributes
                .borrow()
                .iter()
                .map(|a| Rc::new(self.attributes(a)))
                .collect();
        }
    }
}

impl Vm {
    /// `ZCG(preloading)`: whether the preload script of this thread is
    /// running (`opcache_preloading()`).
    #[must_use]
    pub fn is_preloading() -> bool {
        PRELOADING.with(Cell::get)
    }

    /// What preloading made persistent for the running request, if the
    /// request was started from a preload image.
    #[must_use]
    pub fn preload_info() -> Option<Rc<PreloadInfo>> {
        PRELOADED.with(|p| p.borrow().as_ref().map(|p| p.info.clone()))
    }

    /// `accel_preload()`: runs the preload script as the startup request.
    /// [`Self::finish_preload`] then links what it compiled.
    pub fn run_preload(&mut self, src: &[u8], path: &ZStr) -> Result<(), Throw> {
        PRELOADING.with(|p| p.set(true));
        self.run_main(src, path)
    }

    /// Whether `f` is a preloaded function declared again by the
    /// declaration that preloaded it: `preload_remove_declares()` and the
    /// early binding of a preloaded script remove those declarations, so
    /// including a preloaded script again does not redeclare them.
    pub(crate) fn is_preloaded_function(&self, f: &UserFunction) -> bool {
        PRELOADED.with(|p| {
            let p = p.borrow();
            let Some(p) = p.as_ref() else {
                return false;
            };
            let lc = f.name.to_ascii_lowercase();
            p.functions.contains(&lc)
                && matches!(
                    self.functions.get(&lc),
                    Some(Func::User(u)) if u.line_start == f.line_start
                        && u.code.filename.as_bytes() == f.code.filename.as_bytes()
                )
        })
    }

    /// Whether `decl` declares a preloaded class again (see
    /// [`Self::is_preloaded_function`]).
    pub(crate) fn is_preloaded_class(&self, decl: &ClassDecl) -> bool {
        PRELOADED.with(|p| {
            let p = p.borrow();
            let Some(p) = p.as_ref() else {
                return false;
            };
            let lc = decl.name.to_ascii_lowercase();
            p.classes.contains(&lc)
                && self
                    .classes
                    .get(&lc)
                    .is_some_and(|c| same_class_declaration(c, decl))
        })
    }

    /// The first dependency of `decl` that is not declared
    /// (`preload_resolve_deps`): its kind and name.
    fn preload_missing_dependency(&self, decl: &ClassDecl) -> Option<(&'static str, ZStr)> {
        let missing = |n: &ZStr| self.find_class(n.as_bytes()).is_none();
        if let Some(parent) = decl.parent.as_ref().filter(|&p| missing(p)) {
            return Some(("Unknown parent ", parent.clone()));
        }
        if let Some(interface) = decl.interfaces.iter().find(|&i| missing(i)) {
            return Some(("Unknown interface ", interface.clone()));
        }
        if let Some(t) = decl.traits.iter().find(|&t| missing(t)) {
            return Some(("Unknown trait ", t.clone()));
        }
        None
    }

    /// Links one class while preloading, with its errors suppressed
    /// (`preload_error_cb` and `zend_begin_record_errors`); on failure,
    /// the error (message, file, line) and the class left undeclared.
    fn preload_try_link(&mut self, decl: &Rc<ClassDecl>) -> Result<(), (String, ZStr, u32)> {
        let error_reporting = std::mem::replace(&mut self.error_reporting, 0);
        let last_error = self.last_error.take();
        let exit_status = self.exit_status;
        let destructors = crate::object::destructors_enabled();
        let linked = self.declare_class(decl, false);
        let error = match linked {
            Ok(_) => None,
            Err(Throw::Exception(e)) => {
                let message = self.exc_prop(&e, "message");
                let file = match self.exc_prop(&e, "file") {
                    Value::Str(f) => f,
                    _ => decl.filename.clone(),
                };
                let line = match self.exc_prop(&e, "line") {
                    Value::Int(l) => u32::try_from(l).unwrap_or(decl.line_start),
                    _ => decl.line_start,
                };
                let message = match message {
                    Value::Str(m) => m.to_string_lossy(),
                    _ => String::new(),
                };
                Some((message, file, line))
            }
            Err(_) => Some(match self.last_error.take() {
                Some(e) => (
                    String::from_utf8_lossy(&e.message).into_owned(),
                    e.file,
                    e.line,
                ),
                None => (String::new(), decl.filename.clone(), decl.line_start),
            }),
        };
        self.error_reporting = error_reporting;
        self.last_error = last_error;
        self.exit_status = exit_status;
        crate::object::set_destructors_enabled(destructors);
        match error {
            None => Ok(()),
            Some(error) => {
                // A class registered before a failed check is not declared.
                let lc = decl.name.to_ascii_lowercase();
                let registered = self
                    .classes
                    .get(&lc)
                    .is_some_and(|c| c.decl.as_ref().is_some_and(|d| Rc::ptr_eq(d, decl)));
                if registered {
                    self.classes.remove(&lc);
                    self.class_order.retain(|n| *n != lc);
                }
                Err(error)
            }
        }
    }

    /// `preload_link()`: declares what the preload script compiled but
    /// did not declare (the top-level declarations of files compiled with
    /// `opcache_compile_file()`, or not reached), linking classes until no
    /// more can be, then warns about each top-level or anonymous class
    /// left unlinked. Runs after the startup request's shutdown, with its
    /// error handlers and autoloaders gone (no autoloading).
    #[allow(clippy::too_many_lines)]
    fn preload_link(&mut self) {
        self.error_handlers.clear();
        self.autoloaders.clear();
        let units = self.units.clone();
        // Top-level functions are declared when their file is compiled.
        for unit in &units {
            for &i in &unit.hoisted_functions {
                let Some(f) = unit.decls.functions.borrow().get(i as usize).cloned() else {
                    continue;
                };
                let lc = f.name.to_ascii_lowercase();
                if !self.functions.contains_key(&lc) {
                    crate::builtins::info::note_user_function(&lc);
                    self.functions.insert(lc, Func::User(f));
                }
            }
        }
        let mut candidates: Vec<Rc<ClassDecl>> = Vec::new();
        let mut seen: HashSet<usize> = HashSet::new();
        for unit in &units {
            let top = top_level_classes(&unit.main.code);
            let mut found: Vec<Rc<ClassDecl>> = Vec::new();
            for (i, decl) in unit.decls.classes.borrow().iter().enumerate() {
                let anonymous = decl.flags & cflags::ANON != 0;
                let top_level = u32::try_from(i).is_ok_and(|i| top.contains(&i));
                if (anonymous || top_level) && seen.insert(Rc::as_ptr(decl) as usize) {
                    found.push(decl.clone());
                }
            }
            // The class table holds a file's classes in compile (source)
            // order; the declaration table lists the main op array's first.
            found.sort_by_key(|d| d.line_start);
            candidates.extend(found);
        }
        let is_linked = |vm: &Vm, decl: &Rc<ClassDecl>| {
            if decl.flags & cflags::ANON != 0 {
                return vm.bound_decls.contains_key(&(Rc::as_ptr(decl) as usize));
            }
            vm.classes
                .get(&decl.name.to_ascii_lowercase())
                .is_some_and(|c| c.decl.as_ref().is_some_and(|d| Rc::ptr_eq(d, decl)))
        };
        let mut errors: HashMap<usize, (String, ZStr, u32)> = HashMap::new();
        loop {
            let mut changed = false;
            for decl in &candidates {
                // An anonymous class is linked when its `new` runs.
                if decl.flags & cflags::ANON != 0 || is_linked(self, decl) {
                    continue;
                }
                if self.classes.contains_key(&decl.name.to_ascii_lowercase())
                    || self.preload_missing_dependency(decl).is_some()
                {
                    continue;
                }
                match self.preload_try_link(decl) {
                    Ok(()) => changed = true,
                    Err(error) => {
                        errors.insert(Rc::as_ptr(decl) as usize, error);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for decl in &candidates {
            if is_linked(self, decl) {
                continue;
            }
            let anonymous = decl.flags & cflags::ANON != 0;
            let name = printable_class_name(&decl.name);
            let existing = if anonymous {
                None
            } else {
                self.classes.get(&decl.name.to_ascii_lowercase()).cloned()
            };
            let (message, file, line) = if let Some(existing) = existing {
                if same_class_declaration(&existing, decl) {
                    continue;
                }
                (
                    format!("Can't preload already declared class {name}"),
                    decl.filename.clone(),
                    decl.line_start,
                )
            } else if let Some((kind, dep)) = self.preload_missing_dependency(decl) {
                (
                    format!(
                        "Can't preload unlinked class {name}: {kind}{}",
                        dep.to_string_lossy()
                    ),
                    decl.filename.clone(),
                    decl.line_start,
                )
            } else if let Some((message, file, line)) = errors.get(&(Rc::as_ptr(decl) as usize)) {
                (
                    format!("Can't preload unlinked class {name}: {message}"),
                    file.clone(),
                    *line,
                )
            } else {
                // An anonymous class whose dependencies are declared links
                // when it is instantiated.
                continue;
            };
            let _ = self.error_at(crate::errors::level::WARNING, &message, &file, line);
        }
        self.flush_stdout();
    }

    /// The scripts compiled while preloading: the preload script, then the
    /// included and compiled files in order.
    fn preload_scripts(&self) -> Vec<ZStr> {
        let mut scripts = vec![self.script_path.clone()];
        let compiled = self
            .units
            .iter()
            .map(|u| u.main.code.filename.clone())
            .filter(|f| !f.as_bytes().ends_with(b"eval()'d code"));
        for script in self.included.iter().cloned().chain(compiled) {
            if !scripts.iter().any(|s| s.as_bytes() == script.as_bytes()) {
                scripts.push(script);
            }
        }
        scripts
    }

    /// Completes startup shutdown and consumes its executor. Globals, user
    /// constants, closures and shutdown callbacks cannot enter the image.
    pub fn finish_preload(
        mut self,
        result: Result<(), Throw>,
    ) -> Result<PersistentDeclarations, i32> {
        let failed = result.is_err();
        let status = self.shutdown(result);
        if !failed && status == 0 {
            self.preload_link();
        }
        PRELOADING.with(|p| p.set(false));
        // `php_request_shutdown()` leaves per-thread request state behind
        // (a fatal error disables destructors, headers are marked sent with
        // the startup file's output position); the next request starts
        // clean (`zend_activate()`, `sapi_activate()`, `php_output_activate()`).
        crate::object::set_destructors_enabled(true);
        crate::builtins::headers::reset();
        if failed || status != 0 {
            return Err(status.max(1));
        }
        let mut image = ImageBuilder::default();
        let functions: Vec<(ZStr, Rc<UserFunction>)> = self
            .functions
            .iter()
            .filter_map(|(name, f)| match f {
                Func::User(f) => Some((name.clone(), image.function(f))),
                Func::Builtin(_) => None,
            })
            .collect();
        let classes: Vec<Rc<ClassDecl>> = self
            .class_order
            .iter()
            .filter(|name| self.classes.get(*name).is_some_and(|c| c.lc_name == **name))
            .filter_map(|name| {
                self.classes
                    .get(name)?
                    .decl
                    .as_ref()
                    .map(|d| image.class(d))
            })
            .collect();
        // Aliases of user classes, and of internal classes (`class_alias()`
        // of an internal class is preloaded too: GH-18567).
        let aliases = self
            .classes
            .iter()
            .filter(|(name, c)| name.as_bytes() != c.lc_name.as_bytes())
            .map(|(name, c)| (name.clone(), c.lc_name.clone()))
            .collect();
        let units = self.units.iter().map(|unit| image.unit(unit)).collect();
        image.finish();
        // What extensions compile from PHP source while the VM is
        // constructed is not part of the preload script.
        let scripts = self.preload_scripts();
        let from_script = |file: &ZStr| {
            scripts
                .iter()
                .any(|s| file.as_bytes().starts_with(s.as_bytes()))
        };
        let info = PreloadInfo {
            functions: functions
                .iter()
                .filter(|(_, f)| from_script(&f.code.filename))
                .map(|(_, f)| f.name.clone())
                .collect(),
            classes: classes
                .iter()
                .filter(|c| c.flags & cflags::ANON == 0 && from_script(&c.filename))
                .map(|c| c.name.clone())
                .collect(),
            scripts: scripts.clone(),
        };
        Ok(PersistentDeclarations {
            functions,
            classes,
            aliases,
            units,
            halt_offsets: self.ext.halt_offsets.clone(),
            parsed_sources: self.parsed_sources.borrow().clone(),
            info: Rc::new(info),
        })
    }

    /// Installs startup declarations, without executing any startup code,
    /// into a fresh request VM. All function statics and runtime caches are
    /// independent of both the startup executor and other request VMs.
    pub fn install_persistent_declarations(
        &mut self,
        persistent: &PersistentDeclarations,
    ) -> VmResult<()> {
        let mut image = ImageBuilder::default();
        let functions: Vec<_> = persistent
            .functions
            .iter()
            .map(|(name, f)| (name.clone(), image.function(f)))
            .collect();
        let classes: Vec<_> = persistent.classes.iter().map(|c| image.class(c)).collect();
        let units: Vec<_> = persistent.units.iter().map(|u| image.unit(u)).collect();
        image.finish();
        // The startup VM was built like this one, so the image also holds
        // what extensions compile from PHP source while the VM is
        // constructed (e.g. `enum RoundingMode`). This VM already declared
        // those; the preload request itself could not have redeclared them.
        for (name, function) in functions {
            if let std::collections::hash_map::Entry::Vacant(e) = self.functions.entry(name) {
                crate::builtins::info::note_user_function(e.key());
                e.insert(Func::User(function));
            }
        }
        // The startup declaration order has already resolved dependencies.
        // Linking diagnostics were reported by the preload request; opcache
        // installs the linked classes without repeating them.
        let error_reporting = std::mem::replace(&mut self.error_reporting, 0);
        let linked = self.without_declared_observer(|vm| -> VmResult<()> {
            for class in &classes {
                if vm.classes.contains_key(&class.name.to_ascii_lowercase()) {
                    continue;
                }
                let anonymous = class.flags & crate::class::cflags::ANON != 0;
                vm.declare_class(class, anonymous)?;
            }
            Ok(())
        });
        self.error_reporting = error_reporting;
        linked?;
        for (alias, name) in &persistent.aliases {
            if let Some(class) = self.classes.get(name).cloned() {
                self.classes.entry(alias.clone()).or_insert(class);
            }
        }
        self.units.extend(units);
        self.ext
            .halt_offsets
            .extend(persistent.halt_offsets.clone());
        self.parsed_sources
            .borrow_mut()
            .extend(persistent.parsed_sources.clone());
        let lower = |names: &[ZStr]| -> HashSet<ZStr> {
            names.iter().map(ZStr::to_ascii_lowercase).collect()
        };
        let preloaded = Preloaded {
            functions: lower(&persistent.info.functions),
            classes: lower(&persistent.info.classes),
            info: persistent.info.clone(),
        };
        PRELOADED.with(|p| *p.borrow_mut() = Some(preloaded));
        Ok(())
    }
}
