//! Native stacks and context switching for fibers (`Zend/zend_fibers.c`
//! `zend_fiber_init_context` / `zend_fiber_switch_context`, with the
//! assembly of boost.context's `jump_fcontext` reduced to what we need).
//!
//! A [`Coroutine`] owns an `mmap`ed stack with a guard page and runs a
//! closure on it. [`Coroutine::resume`] switches from the current stack to
//! the coroutine; [`Suspender::suspend`] switches back. Only the callee-saved
//! registers (plus the SSE/x87 control words) are saved, because a switch is
//! an ordinary function call as far as the compiler is concerned.
//!
//! This is the one place in the VM that needs `unsafe`: switching stacks
//! cannot be expressed in safe Rust. The invariants are documented on each
//! block; they boil down to "a coroutine's stack is only executed between a
//! `resume` and the matching `suspend`, and is unmapped only after the
//! closure has returned or when the coroutine is abandoned while suspended".

use std::any::Any;
use std::cell::Cell;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

/// The closure run by a coroutine.
type Body = Box<dyn FnOnce(&Suspender)>;

/// Shared state between the resumer and the coroutine. Boxed so that its
/// address is stable: the switch routine stores stack pointers into it.
struct Inner {
    stack: Stack,
    /// Stack pointer of the coroutine while it is not running.
    sp: Cell<*mut u8>,
    /// Stack pointer of the resumer while the coroutine runs.
    caller_sp: Cell<*mut u8>,
    body: Cell<Option<Body>>,
    finished: Cell<bool>,
    panic: Cell<Option<Box<dyn Any + Send>>>,
}

/// A closure running on its own native stack.
pub(crate) struct Coroutine {
    inner: Box<Inner>,
}

impl std::fmt::Debug for Coroutine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coroutine")
            .field("stack_size", &self.inner.stack.len)
            .field("finished", &self.inner.finished.get())
            .finish()
    }
}

/// Handle used by the code running inside a coroutine to switch back to
/// its resumer.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Suspender {
    inner: *const Inner,
}

impl Suspender {
    /// Switches back to the resumer; returns when the coroutine is resumed.
    ///
    /// Must only be called from the coroutine's own stack while it runs.
    pub(crate) fn suspend(self) {
        // SAFETY: a `Suspender` is only handed to the body of the coroutine
        // it belongs to, and the resumer keeps the `Coroutine` (hence the
        // boxed `Inner`) alive for as long as the body can run.
        let inner = unsafe { &*self.inner };
        // SAFETY: we are running on the coroutine's stack (precondition), so
        // saving the current stack pointer into `sp` and jumping to the
        // resumer's saved `caller_sp` is the documented switch protocol.
        unsafe { arch::switch(inner.sp.as_ptr(), inner.caller_sp.get()) };
    }
}

/// Smallest stack PHP accepts (`zend_fiber_stack_allocate`): one usable
/// page plus the guard page.
pub(crate) const MIN_STACK: usize = 2 * PAGE;

const PAGE: usize = 4096;

/// Why a coroutine could not be created.
#[derive(Debug)]
pub(crate) enum StackError {
    /// The requested size is below [`MIN_STACK`].
    TooSmall,
    /// `mmap` failed (message as PHP formats it: `strerror (errno)`).
    Mmap(String),
    /// Context switching is not implemented for this platform.
    Unsupported,
}

impl Coroutine {
    /// Creates a coroutine with a stack of `size` usable bytes that will
    /// run `body` when first resumed.
    pub(crate) fn new(size: usize, body: Body) -> Result<Self, StackError> {
        if size < MIN_STACK {
            return Err(StackError::TooSmall);
        }
        if !arch::SUPPORTED {
            return Err(StackError::Unsupported);
        }
        let stack = Stack::new(size)?;
        let inner = Box::new(Inner {
            stack,
            sp: Cell::new(std::ptr::null_mut()),
            caller_sp: Cell::new(std::ptr::null_mut()),
            body: Cell::new(Some(body)),
            finished: Cell::new(false),
            panic: Cell::new(None),
        });
        let top = inner.stack.top();
        let arg: *const Inner = std::ptr::from_ref::<Inner>(&inner);
        // SAFETY: `top` is the 16-byte aligned end of a writable mapping of
        // at least `MIN_STACK - PAGE` bytes, enough for the initial frame.
        let sp = unsafe { arch::init_stack(top, entry as *const () as usize, arg as usize) };
        inner.sp.set(sp);
        Ok(Coroutine { inner })
    }

    /// Runs the coroutine until it suspends or its body returns. Returns
    /// `true` once the body has returned. A panic in the body is resumed on
    /// the caller's stack.
    pub(crate) fn resume(&self) -> bool {
        let inner = &*self.inner;
        if inner.finished.get() {
            return true;
        }
        // SAFETY: the coroutine is not running (we are on another stack and
        // the switch protocol is strictly nested), `sp` holds the stack
        // pointer saved by `init_stack` or by its last `suspend`, and
        // `inner` outlives the switch because `self` is borrowed for it.
        unsafe { arch::switch(inner.caller_sp.as_ptr(), inner.sp.get()) };
        if let Some(p) = inner.panic.take() {
            resume_unwind(p);
        }
        inner.finished.get()
    }

    /// Whether the body has returned.
    #[cfg(test)]
    fn finished(&self) -> bool {
        self.inner.finished.get()
    }
}

/// First function executed on a coroutine stack (called by the
/// architecture trampoline with the `Inner` pointer).
extern "C" fn entry(inner: *const Inner) {
    // SAFETY: `init_stack` was given the address of the boxed `Inner`,
    // which outlives the body (see `Coroutine::resume`).
    let inner = unsafe { &*inner };
    let susp = Suspender { inner };
    if let Some(body) = inner.body.take() {
        if let Err(p) = catch_unwind(AssertUnwindSafe(|| body(&susp))) {
            inner.panic.set(Some(p));
        }
    }
    inner.finished.set(true);
    // SAFETY: we are on the coroutine stack; switch back for good. The
    // resumer never switches to a finished coroutine again.
    unsafe { arch::switch(inner.sp.as_ptr(), inner.caller_sp.get()) };
    // Unreachable: abort rather than return into the trampoline.
    std::process::abort();
}

/// An `mmap`ed stack with a `PROT_NONE` guard page at its low end.
struct Stack {
    base: *mut u8,
    len: usize,
}

mod sys {
    use std::ffi::c_void;
    pub const PROT_NONE: i32 = 0;
    pub const PROT_READ: i32 = 1;
    pub const PROT_WRITE: i32 = 2;
    pub const MAP_PRIVATE: i32 = 0x02;
    pub const MAP_ANONYMOUS: i32 = 0x20;
    pub const MAP_NORESERVE: i32 = 0x4000;
    pub const MAP_STACK: i32 = 0x20000;
    extern "C" {
        pub fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: i32,
            flags: i32,
            fd: i32,
            off: i64,
        ) -> *mut c_void;
        pub fn munmap(addr: *mut c_void, len: usize) -> i32;
        pub fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
    }
}

impl Stack {
    fn new(size: usize) -> Result<Self, StackError> {
        let len = size.div_ceil(PAGE) * PAGE + PAGE;
        // SAFETY: an anonymous private mapping at a kernel-chosen address
        // does not alias any Rust object.
        let p = unsafe {
            sys::mmap(
                std::ptr::null_mut(),
                len,
                sys::PROT_READ | sys::PROT_WRITE,
                sys::MAP_PRIVATE | sys::MAP_ANONYMOUS | sys::MAP_NORESERVE | sys::MAP_STACK,
                -1,
                0,
            )
        };
        if p as isize == -1 {
            return Err(StackError::Mmap(os_error()));
        }
        // SAFETY: `p` is the start of the mapping we just created.
        if unsafe { sys::mprotect(p, PAGE, sys::PROT_NONE) } != 0 {
            let e = os_error();
            // SAFETY: unmapping the mapping created above.
            unsafe { sys::munmap(p, len) };
            return Err(StackError::Mmap(e));
        }
        Ok(Stack {
            base: p.cast(),
            len,
        })
    }

    /// The (16-byte aligned) highest address of the stack.
    fn top(&self) -> *mut u8 {
        self.base.wrapping_add(self.len)
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        // SAFETY: `base`/`len` describe a mapping created by `Stack::new`.
        // A stack is only dropped when its coroutine is not running.
        unsafe { sys::munmap(self.base.cast(), self.len) };
    }
}

/// `strerror(errno) (errno)` as `zend_fiber_stack_allocate` prints it.
fn os_error() -> String {
    let e = std::io::Error::last_os_error();
    let code = e.raw_os_error().unwrap_or(0);
    let s = e.to_string();
    let msg = s.split(" (os error").next().unwrap_or(&s).to_string();
    format!("{msg} ({code})")
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod arch {
    pub const SUPPORTED: bool = true;

    // Callee-saved state per the System V ABI: rbx, rbp, r12-r15, the
    // MXCSR control bits and the x87 control word.
    std::arch::global_asm!(
        ".text",
        ".p2align 4",
        ".globl phprust_fiber_switch",
        ".type phprust_fiber_switch,@function",
        "phprust_fiber_switch:",
        ".cfi_startproc",
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "sub rsp, 8",
        "stmxcsr [rsp]",
        "fnstcw [rsp + 4]",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "ldmxcsr [rsp]",
        "fldcw [rsp + 4]",
        "add rsp, 8",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
        ".cfi_endproc",
        ".size phprust_fiber_switch, .-phprust_fiber_switch",
        "",
        ".p2align 4",
        ".globl phprust_fiber_trampoline",
        ".type phprust_fiber_trampoline,@function",
        "phprust_fiber_trampoline:",
        ".cfi_startproc",
        ".cfi_undefined rip",
        "mov rdi, r12",
        "call r13",
        "ud2",
        ".cfi_endproc",
        ".size phprust_fiber_trampoline, .-phprust_fiber_trampoline",
    );

    extern "C" {
        fn phprust_fiber_switch(save: *mut *mut u8, to: *mut u8);
        fn phprust_fiber_trampoline();
    }

    /// Saves the current context, storing its stack pointer in `*save`,
    /// and resumes the context whose stack pointer is `to`.
    ///
    /// # Safety
    /// `to` must be a stack pointer produced by [`init_stack`] or saved by
    /// a previous `switch` whose context has not been resumed since.
    pub unsafe fn switch(save: *mut *mut u8, to: *mut u8) {
        // SAFETY: forwarded preconditions.
        unsafe { phprust_fiber_switch(save, to) }
    }

    /// Lays out an initial frame on a fresh stack so that the first
    /// `switch` to it calls `entry(arg)` through the trampoline.
    ///
    /// # Safety
    /// `top` must be the 16-byte aligned end of a writable region of at
    /// least 96 bytes.
    pub unsafe fn init_stack(top: *mut u8, entry: usize, arg: usize) -> *mut u8 {
        // Slots (from the new sp upwards): MXCSR + x87 CW, r15, r14, r13,
        // r12, rbx, rbp, return address. After `ret` the stack pointer is
        // `sp + 64`, 16-byte aligned as required before the `call`.
        #[allow(clippy::cast_ptr_alignment)] // `top` is 16-byte aligned
        let sp = top.wrapping_sub(16 + 64).cast::<usize>();
        let slots: [usize; 8] = [
            0x037F_0000_1F80, // MXCSR default | x87 CW default << 32
            0,
            0,
            entry,
            arg,
            0,
            0,
            phprust_fiber_trampoline as *const () as usize,
        ];
        for (i, v) in slots.into_iter().enumerate() {
            // SAFETY: within the region guaranteed by the caller.
            unsafe { sp.add(i).write(v) };
        }
        sp.cast()
    }
}

#[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
mod arch {
    pub const SUPPORTED: bool = false;

    /// Never called: [`SUPPORTED`] is false.
    pub unsafe fn switch(_save: *mut *mut u8, _to: *mut u8) {
        std::process::abort();
    }

    /// Never called: [`SUPPORTED`] is false.
    pub unsafe fn init_stack(top: *mut u8, _entry: usize, _arg: usize) -> *mut u8 {
        top
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn ping_pong() {
        let log = Rc::new(std::cell::RefCell::new(Vec::new()));
        let l2 = log.clone();
        let co = Coroutine::new(
            256 * 1024,
            Box::new(move |s: &Suspender| {
                for i in 0..3 {
                    l2.borrow_mut().push(i);
                    s.suspend();
                }
            }),
        )
        .expect("coroutine");
        let mut n = 0;
        while !co.resume() {
            log.borrow_mut().push(100 + n);
            n += 1;
        }
        assert!(co.finished());
        assert_eq!(*log.borrow(), vec![0, 100, 1, 101, 2, 102]);
    }

    #[test]
    fn nested_coroutines() {
        let out = Rc::new(Cell::new(0u32));
        let o2 = out.clone();
        let co = Coroutine::new(
            256 * 1024,
            Box::new(move |s: &Suspender| {
                let o3 = o2.clone();
                let inner = Coroutine::new(
                    256 * 1024,
                    Box::new(move |s2: &Suspender| {
                        o3.set(o3.get() + 1);
                        s2.suspend();
                        o3.set(o3.get() + 10);
                    }),
                )
                .expect("inner");
                assert!(!inner.resume());
                s.suspend();
                assert!(inner.resume());
                o2.set(o2.get() + 100);
            }),
        )
        .expect("outer");
        assert!(!co.resume());
        assert_eq!(out.get(), 1);
        assert!(co.resume());
        assert_eq!(out.get(), 111);
    }

    #[test]
    fn float_math_and_panics_cross_the_switch() {
        let co = Coroutine::new(
            256 * 1024,
            Box::new(|s: &Suspender| {
                let x = 1.5f64.sqrt();
                s.suspend();
                assert!((x * x - 1.5).abs() < 1e-12);
                panic!("boom");
            }),
        )
        .expect("coroutine");
        assert!(!co.resume());
        let r = catch_unwind(AssertUnwindSafe(|| co.resume()));
        assert!(r.is_err());
    }

    #[test]
    fn too_small() {
        assert!(matches!(
            Coroutine::new(100, Box::new(|_: &Suspender| {})),
            Err(StackError::TooSmall)
        ));
    }

    #[test]
    fn abandoned_coroutine_is_unmapped() {
        let co =
            Coroutine::new(64 * 1024, Box::new(|s: &Suspender| s.suspend())).expect("coroutine");
        assert!(!co.resume());
        drop(co);
    }
}
