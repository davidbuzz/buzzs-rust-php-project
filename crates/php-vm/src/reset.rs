// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Request reset restoring clean VM state across request boundaries
//! without rebuilding the VM (for persistent worker SAPIs such as mod_php).

use crate::class::cflags;
use crate::func::Func;
use crate::vm::Vm;
use php_runtime::{Value, ZStr};
use std::cell::RefCell;
use std::rc::Rc;

impl Vm {
    /// `zend_deactivate()` after `shutdown_executor()`, for a SAPI that
    /// drops the VM once the request is over (CGI, FPM):
    /// `zend_ini_deactivate()` restores every directive the request
    /// modified, in the order they were first modified, through the
    /// entry's handlers at the deactivate stage. Their diagnostics (a
    /// `max_memory_limit` below what the request still holds,
    /// `zlib.output_compression` against an `output_handler` not yet
    /// restored) are reported as C reports them: "in Unknown on line 0",
    /// from "PHP Request Shutdown", and with no fatal error backtrace
    /// (GHSA-2fjv-p558-g82m). Each handler runs in its own `zend_try`: one
    /// that bails out does not stop the others.
    pub fn ini_deactivate(&mut self) {
        use php_support::ini::Stage;
        self.in_shutdown = true;
        self.executor_active = false;
        for name in self.ini.modified_names() {
            let memory = name == "memory_limit" || name == "max_memory_limit";
            if memory {
                let usage = self.end_usage.max(php_memlimit::real_usage());
                self.ini.memory_usage = u64::try_from(usage).unwrap_or(u64::MAX);
            }
            let mut sink: Vec<php_support::Diagnostic> = Vec::new();
            self.ini.restore(&name, Stage::Deactivate, &mut sink);
            for d in sink {
                let level = i64::from(d.level.0);
                let _ = if d.docref {
                    self.docref(level, &d.message)
                } else {
                    self.error(level, &d.message)
                };
            }
            // The limit itself is left alone: the request's allocations
            // may still exceed the restored value ("set the memory limit
            // when the memory manager has been shut down").
            if !memory {
                self.sync_ini(&name);
            }
            if let Some(h) = self.ini_modify_hooks.get(name.as_str()).copied() {
                let v = self.ini.get(&name).map(<[u8]>::to_vec).unwrap_or_default();
                self.ini_stage = crate::ext::IniStage::Restore;
                let _ = h(self, &v);
                self.ini_stage = crate::ext::IniStage::Runtime;
            }
        }
    }

    /// Restores clean start-of-request state on a persistent VM instance:
    /// drops user functions, classes and constants, resets superglobals,
    /// statics, autoloaders, error handlers, output buffers, headers, INI,
    /// handle counter and extension state.
    #[allow(clippy::too_many_lines)] // php_request_shutdown()'s steps, in order
    pub fn request_reset(&mut self) {
        // Ensure request shutdown hooks ran if not already completed.
        if !self.in_shutdown {
            self.in_shutdown = true;
            self.run_shutdown_hooks();
        }
        // The next request starts with the executor up (`zend_activate()`),
        // whatever this one's shutdown (`Vm::ini_deactivate`) left.
        self.in_shutdown = false;
        self.executor_active = true;

        // 1. User functions: clear static variables, then retain only internal functions.
        for f in self.functions.values() {
            if let Func::User(u) = f {
                u.statics.borrow_mut().clear();
            }
        }
        self.functions.retain(|_, f| !f.is_user());
        crate::func::bump_func_table_version();

        // 2. Classes: clear user class static props, constants, defaults, enum cases,
        // and retain only internal classes.
        for c in self.classes.values() {
            if c.flags & cflags::INTERNAL != 0 {
                continue;
            }
            for m in c.methods.values() {
                if let Func::User(u) = &m.func {
                    u.statics.borrow_mut().clear();
                }
            }
            for p in &c.static_props {
                *p.cell.borrow_mut() = Value::Null;
            }
            for k in &c.consts {
                *k.value.borrow_mut() = None;
            }
            *c.defaults.borrow_mut() = None;
            c.enum_cases.borrow_mut().clear();
        }
        // An internal class stays under its own name only: an alias of one
        // (`class_alias('stdClass', 'foo')`) is the request's, like every
        // entry php-src's `shutdown_executor()` discards past the startup
        // classes. Kept, it would make the next request's `class foo {}`
        // "Cannot redeclare class stdClass".
        self.classes.retain(|key, c| {
            c.flags & cflags::INTERNAL != 0
                && key.as_bytes().eq_ignore_ascii_case(c.name.as_bytes())
        });
        self.class_order.retain(|name| {
            self.classes
                .get(name)
                .is_some_and(|c| c.flags & cflags::INTERNAL != 0)
        });

        // Reset internal class static properties and constants to initial defaults.
        for c in self.classes.values() {
            if c.flags & cflags::INTERNAL != 0 {
                for p in &c.static_props {
                    if let Some(crate::class::ConstExpr::Value(init)) = &p.default {
                        *p.cell.borrow_mut() = init.clone();
                    } else {
                        *p.cell.borrow_mut() = Value::Null;
                    }
                    p.initialized.set(true);
                }
                for k in &c.consts {
                    *k.value.borrow_mut() = None;
                }
                *c.defaults.borrow_mut() = None;
                // `Closure`'s first-class-callable cache (`f(...)`, see
                // `closure.rs`): its closures are the request's objects, and
                // a function of the next request may have the same name.
                c.enum_cases.borrow_mut().clear();
            }
        }

        // Clear bound class declarations, units, parsed sources, anonymous counter.
        self.bound_decls.clear();
        self.units.clear();
        self.parsed_sources.borrow_mut().clear();
        self.anon_counter = 0;

        // 3. User constants: remove all user-defined constants.
        for name in std::mem::take(&mut self.user_constants) {
            self.constants.remove(&name);
            self.const_attributes.remove(&name);
            self.const_files.remove(&name);
            self.const_names.remove(&name);
        }
        self.const_deprecation_guard.clear();
        self.ct_constants = None;
        *self.const_deprecation_filter.borrow_mut() = (0, Vec::new());

        // 4. Superglobals: fresh symbol table.
        self.globals = Rc::new(RefCell::new(crate::vm::VarStore::default()));

        // 5. Autoloaders.
        self.autoloaders.clear();
        self.autoloading.clear();

        // 6. Error & exception handlers.
        self.error_handlers.clear();
        self.exception_handlers.clear();
        self.in_error_handler = 0;
        self.last_error = None;
        self.last_fatal_error_backtrace = None;
        self.error_msg_html_ready = false;
        self.reporting_oom = false;
        self.request_ending = false;

        // 7. Output buffers.
        self.output = php_support::output::OutputStack::new();
        self.output_handler_threw = false;

        // 8. Headers.
        self.headers_sent = false;
        self.response_mimetype = None;
        self.send_default_content_type = true;
        crate::builtins::headers::reset();

        // 9. INI settings: revert alterations back to server configuration.
        let mut sink: Vec<php_support::Diagnostic> = Vec::new();
        self.ini.deactivate(&mut sink);
        for n in ["error_reporting", "precision", "serialize_precision"] {
            self.sync_ini(n);
        }

        // 10. Handle counter, object store and collector: emptied last
        // (below), once nothing of the request holds an object any more.

        // 11. Extensions and runtime subsystems.
        crate::auto_globals::reset();
        crate::weakref::reset();
        crate::lazy::reset();
        crate::typed_refs::reset();
        crate::builtins::dump::reset();
        crate::interrupt::clear_interrupt();
        crate::object::set_destructors_enabled(true);
        // `zend_init_rsrc_list()`: every request numbers its resources from
        // `Resource id #1` (the request's streams were closed at shutdown).
        php_runtime::Resource::set_next_handle(1);

        // 12. Execution state.
        self.frames.clear();
        self.included.clear();
        self.included_set.clear();
        self.attribute_call_sites.clear();
        self.exit_status = 0;
        self.shutdown_functions.clear();
        self.pending_throw = None;
        self.script_path = ZStr::empty();
        self.argv.clear();
        self.depth = 0;
        self.once_flags.clear();
        self.tick_functions.clear();
        self.user_ticks.clear();
        self.ticks_count = 0;
        self.fiber_switch_block = 0;
        self.timeout_seconds = 0;
        self.rfc1867_uploaded_files.clear();
        self.fibers.clear();
        self.pending_call_dst = None;
        self.builtin_deprecation_reported = false;
        self.dim_ctx = crate::ext::DimensionCtx::Read;
        self.prop_ctx = crate::ext::PropCtx::Read;
        self.ini_stage = crate::ext::IniStage::Runtime;
        self.request_info = php_sapi::RequestInfo::default();
        self.magic_call_name = None;
        self.main_return = Value::Null;
        self.call_bufs = crate::vm::CallBufs::prewarmed(&self.empty_code);
        self.prop_guards.clear();
        self.observer = crate::observer::ObserverState::default();
        self.user_opcode_handlers.clear();
        self.ext = crate::builtins::ExtState::default();
        // What a request in progress holds, as a fresh VM starts it: a
        // throw the tape left for its caller (an out-of-memory error in a
        // shutdown function), the include being opened, a trampoline being
        // entered, a write walk's variable, a resolved callable argument.
        self.tape_throw = None;
        self.bailouts = 0;
        self.including = None;
        self.trampoline_entry = None;
        self.cv_root = None;
        self.checked_callable = None;
        // Last: an aborted shutdown (a fatal error in a shutdown function)
        // leaves the request's objects to the drops above, which put their
        // handles back on the store's free list. What the request leaked to
        // cycles is freed with it, then the collector starts over
        // (`gc_reset()`), without the roots those releases buffered.
        crate::object::free_object_storage();
        crate::gc::request_startup(self.ini.get_bool("zend.enable_gc"));
        crate::object::reset_store();
    }
}

#[cfg(test)]
mod tests {
    use crate::{Vm, VmOptions};
    use php_runtime::ZStr;
    use std::cell::RefCell;
    use std::io::Write;
    use std::rc::Rc;

    #[derive(Clone, Default)]
    struct Sink(Rc<RefCell<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// An alias of an internal class is the request's: kept, the next
    /// request's `class foo {}` failed with "Cannot redeclare class
    /// stdClass" (a long-running php -S, after
    /// `Zend/tests/class_alias/class_alias_006.phpt`).
    #[test]
    fn request_reset_drops_aliases_of_internal_classes() {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(b"<?php class_alias('stdclass', 'foo');", "req1.php");
        assert!(vm.classes.contains_key(&ZStr::from("foo")));
        vm.request_reset();
        assert!(!vm.classes.contains_key(&ZStr::from("foo")), "alias kept");
        assert!(
            vm.classes.contains_key(&ZStr::from("stdclass")),
            "stdClass dropped"
        );
        sink.0.borrow_mut().clear();
        vm.run_script(b"<?php class foo {} echo get_class(new foo);", "req2.php");
        assert_eq!(sink.0.borrow().as_slice(), b"foo");
    }

    /// `f(...)` of the next request's `func` is not the closure the cache
    /// kept for the previous request's `func` (Zend/tests/closures/
    /// `closure_061` then `gh8083` on one php -S).
    #[test]
    fn request_reset_empties_the_first_class_callable_cache() {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(b"<?php function func($arg) {} $c = func(...);", "req1.php");
        vm.request_reset();
        sink.0.borrow_mut().clear();
        vm.run_script(
            b"<?php function func() {} echo (new ReflectionFunction(func(...)))->getNumberOfParameters();",
            "req2.php",
        );
        assert_eq!(sink.0.borrow().as_slice(), b"0");
    }

    /// What a request leaked to cycles (through properties, a reference to
    /// the object itself, an array holding a reference to itself) is freed
    /// by the reset, and the next request's collector finds none of it
    /// (`gc_collect_cycles()` gave 60 on the php -S that ran such a
    /// script before; php-src frees it with the request's memory).
    #[test]
    fn request_reset_frees_what_the_request_leaked_to_cycles() {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(
            b"<?php class N { public $p; }
            for ($i = 0; $i < 3; $i++) {
                $a = new N; $b = new N; $a->p = $b; $b->p = $a;
                $o = new N; $o->p = &$o;
                $r = [new N]; $r[] = &$r;
            }
            unset($a, $b, $o, $r);",
            "req1.php",
        );
        let leaked: Vec<php_runtime::ObjWeak> = crate::object::live_objects()
            .iter()
            .map(php_runtime::Obj::downgrade)
            .collect();
        assert_eq!(leaked.len(), 8, "three pairs and the last two self-cycles");
        vm.request_reset();
        assert!(
            leaked.iter().all(|w| w.upgrade().is_none()),
            "a cycle outlived its request"
        );
        sink.0.borrow_mut().clear();
        vm.run_script(b"<?php echo gc_collect_cycles();", "req2.php");
        assert_eq!(sink.0.borrow().as_slice(), b"0");
    }

    #[test]
    fn request_reset_drops_user_declarations_and_resets_state() {
        let sink = Sink::default();
        let mut vm = Vm::new(&VmOptions::default(), Box::new(sink.clone()));
        vm.run_script(
            b"<?php
            function test_user_fn() { static $s = 1; return ++$s; }
            class TestUserClass { public static $prop = 42; }
            define('TEST_USER_CONST', 'hello');
            $test_global = 'foo';
            $obj = new stdClass;
            ini_set('precision', '20');
            ",
            "req1.php",
        );

        assert!(vm.functions.get(&ZStr::from("test_user_fn")).is_some());
        assert!(vm.classes.get(&ZStr::from("testuserclass")).is_some());
        assert!(vm.constants.get(&ZStr::from("TEST_USER_CONST")).is_some());
        assert_eq!(vm.precision, 20);

        vm.request_reset();

        assert!(
            vm.functions.get(&ZStr::from("test_user_fn")).is_none(),
            "user function must be dropped"
        );
        assert!(
            vm.classes.get(&ZStr::from("testuserclass")).is_none(),
            "user class must be dropped"
        );
        assert!(
            vm.constants.get(&ZStr::from("TEST_USER_CONST")).is_none(),
            "user constant must be dropped"
        );
        assert_eq!(vm.precision, 14, "INI must be restored");
        assert!(
            vm.globals.borrow().slots.is_empty(),
            "globals must be empty"
        );

        // Run a second script on the same persistent VM: object handle starts at #1
        sink.0.borrow_mut().clear();
        vm.run_script(
            b"<?php
            $o = new stdClass;
            var_dump($o);
            ",
            "req2.php",
        );
        let out = String::from_utf8_lossy(&sink.0.borrow()).into_owned();
        assert!(
            out.contains("object(stdClass)#1"),
            "handle counter must start at #1, got: {out}"
        );
    }
}
