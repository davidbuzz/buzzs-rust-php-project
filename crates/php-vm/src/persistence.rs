//! Declaration promotion across a startup/request boundary. Only compiled
//! declarations are retained; executor values and linked class caches are not.

use crate::class::{AttributeInfo, ClassDecl, ConstExpr, PropHooks};
use crate::compiler::CompiledUnit;
use crate::func::{Func, UserFunction};
use crate::opcode::{DeclTable, OpArray, ParamInfo};
use crate::{Throw, Vm, VmResult};
use php_runtime::ZStr;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Immutable declaration image produced by a completed preload request.
/// Installation makes an independent graph for every subsequent request.
pub struct PersistentDeclarations {
    functions: Vec<(ZStr, Rc<UserFunction>)>,
    classes: Vec<Rc<ClassDecl>>,
    aliases: Vec<(ZStr, ZStr)>,
    units: Vec<Rc<CompiledUnit>>,
    halt_offsets: HashMap<ZStr, i64>,
    parsed_sources: HashMap<ZStr, Rc<php_parser::Program>>,
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
    /// Completes startup shutdown and consumes its executor. Globals, user
    /// constants, closures and shutdown callbacks cannot enter the image.
    pub fn finish_preload(
        mut self,
        result: Result<(), Throw>,
    ) -> Result<PersistentDeclarations, i32> {
        let failed = result.is_err();
        let status = self.shutdown(result);
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
        let functions = self
            .functions
            .iter()
            .filter_map(|(name, f)| match f {
                Func::User(f) => Some((name.clone(), image.function(f))),
                Func::Builtin(_) => None,
            })
            .collect();
        let classes = self
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
        let aliases = self
            .classes
            .iter()
            .filter(|(name, c)| c.decl.is_some() && name.as_bytes() != c.lc_name.as_bytes())
            .map(|(name, c)| (name.clone(), c.lc_name.clone()))
            .collect();
        let units = self.units.iter().map(|unit| image.unit(unit)).collect();
        image.finish();
        Ok(PersistentDeclarations {
            functions,
            classes,
            aliases,
            units,
            halt_offsets: self.ext.halt_offsets.clone(),
            parsed_sources: self.parsed_sources.borrow().clone(),
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
            self.functions.entry(name).or_insert(Func::User(function));
        }
        // The startup declaration order has already resolved dependencies.
        // Linking diagnostics were reported by the preload request; opcache
        // installs the linked classes without repeating them.
        let error_reporting = std::mem::replace(&mut self.error_reporting, 0);
        let mut linked = Ok(());
        for class in &classes {
            if self.classes.contains_key(&class.name.to_ascii_lowercase()) {
                continue;
            }
            let anonymous = class.flags & crate::class::cflags::ANON != 0;
            if let Err(error) = self.declare_class(class, anonymous) {
                linked = Err(error);
                break;
            }
        }
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
        Ok(())
    }
}
