//! Destructors of garbage run inside a fiber (`gc_call_destructors_in_fiber`,
//! `gc_destructor_fiber`).
//!
//! When the collector runs while a fiber is active, a destructor may call
//! `Fiber::suspend()`. php-src therefore runs the destructors in a fiber of
//! its own: if a destructor suspends it, that fiber is abandoned (whoever
//! resumes it later finishes that one destructor) and a new fiber continues
//! with the remaining ones.

use crate::func::Args;
use crate::vm::{Throw, Vm, VmResult};
use php_runtime::{Obj, PhpObject, Value};
use std::cell::{Cell, RefCell};
use std::rc::Weak;

/// The destructor queue shared with the destructor fiber.
#[derive(Default)]
struct Queue {
    dtors: Vec<Weak<dyn PhpObject>>,
    next: usize,
    /// Generation of the fiber currently owned by the collector.
    owner: u64,
    /// All destructors have been called.
    done: bool,
    err: Option<Throw>,
    zombies: Vec<Obj>,
}

thread_local! {
    static QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    /// Generation handed to the fiber being started.
    static STARTING: Cell<u64> = const { Cell::new(0) };
}

/// The name the function has in backtraces.
const FN_NAME: &str = "gc_destructor_fiber";

/// The body of a destructor fiber (`ZEND_FUNCTION(gc_destructor_fiber)`).
fn gc_destructor_fiber(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let me = STARTING.with(Cell::get);
    loop {
        let next = QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            if q.owner != me {
                return None;
            }
            let w = q.dtors.get(q.next).cloned();
            if w.is_some() {
                q.next += 1;
            } else {
                q.done = true;
            }
            w
        });
        let Some(w) = next else {
            return Ok(Value::Null);
        };
        let mut err = QUEUE.with(|q| q.borrow_mut().err.take());
        let r = vm.gc_destruct_one(&w, &mut err);
        // A destructor that suspended this fiber may finish long after the
        // collection; its outcome then belongs to whoever resumed it.
        let still_owned = QUEUE.with(|q| q.borrow().owner == me);
        if !still_owned {
            return match (r, err) {
                (Err(e), _) | (Ok(_), Some(e)) => Err(e),
                (Ok(_), None) => Ok(Value::Null),
            };
        }
        let zombie = r?;
        QUEUE.with(|q| {
            let mut q = q.borrow_mut();
            q.err = err;
            q.zombies.extend(zombie);
        });
    }
}

impl Vm {
    /// Starts destructor fibers until every destructor of `dtors` has been
    /// called (or handed to a suspended fiber).
    pub(super) fn gc_call_destructors_in_fiber(
        &mut self,
        dtors: &[Weak<dyn PhpObject>],
        err: &mut Option<Throw>,
        zombies: &mut Vec<Obj>,
    ) -> VmResult<()> {
        let saved = QUEUE.with(|q| {
            std::mem::replace(
                &mut *q.borrow_mut(),
                Queue {
                    dtors: dtors.to_vec(),
                    err: err.take(),
                    ..Queue::default()
                },
            )
        });
        let r = self.gc_run_destructor_fibers();
        let q = QUEUE.with(|q| std::mem::replace(&mut *q.borrow_mut(), saved));
        *err = q.err;
        zombies.extend(q.zombies);
        r
    }

    fn gc_run_destructor_fibers(&mut self) -> VmResult<()> {
        let Some(cls) = self.find_class(b"Fiber") else {
            return Ok(());
        };
        loop {
            let generation = GENERATION.with(|g| {
                g.set(g.get() + 1);
                g.get()
            });
            QUEUE.with(|q| q.borrow_mut().owner = generation);
            // The function is reachable only through this fiber.
            let lc = crate::builtins::reg(self, &format!("{FN_NAME}(): void"), gc_destructor_fiber);
            let fiber = self.new_object_with_args(&cls, vec![Value::string(FN_NAME)]);
            self.functions.remove(&lc);
            let fiber = fiber?;
            STARTING.with(|s| s.set(generation));
            // `zend_fiber_start`, not `Fiber->start()`: the destructors'
            // backtraces continue with the frame that ran the collection.
            self.fiber_start_internal(&fiber)?;
            let done = QUEUE.with(|q| {
                let mut q = q.borrow_mut();
                if !q.done {
                    // A destructor suspended the fiber: abandon it.
                    q.owner = 0;
                }
                q.done
            });
            if done {
                return Ok(());
            }
            // `zend_object_release(&fiber->std)`: unless the application
            // kept it, the abandoned fiber is destroyed now, before the next
            // one starts; what its unwinding throws is remembered with the
            // destructors' exceptions (`remember_prev_exception`).
            drop(fiber);
            if let Err(e) = self.run_pending_destructors() {
                remember_exception(e)?;
            }
        }
    }
}

/// `remember_prev_exception`: chains an exception thrown while the
/// collector switches destructor fibers to the ones already collected.
/// Other unwinds (exit, fatal errors) are returned.
fn remember_exception(e: Throw) -> VmResult<()> {
    let Throw::Exception(new) = e else {
        return Err(e);
    };
    QUEUE.with(|q| {
        let mut q = q.borrow_mut();
        if let Some(Throw::Exception(old)) = &q.err {
            crate::exceptions::chain_previous(&new, old);
        }
        q.err = Some(Throw::Exception(new));
    });
    Ok(())
}
