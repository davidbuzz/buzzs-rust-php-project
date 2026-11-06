//! A model of `ZEND_CHECK_STACK_LIMIT` for the recursive array functions
//! (`count(..., COUNT_RECURSIVE)`, `array_walk_recursive()`,
//! `array_replace_recursive()`, `compact()`).
//!
//! php-stdarr recurses on the Rust stack, which is far larger than PHP's.
//! When a script sets `zend.max_allowed_stack_size`, the nesting depth of
//! the input is measured up front (iteratively) and compared with the
//! depth PHP's C stack would allow, so the same "Maximum call stack size"
//! `Error` is thrown before any work is done.

use php_runtime::{Value, ZArray};
use php_vm::ext::VmResult;
use php_vm::Vm;

/// Approximate C stack bytes used per nesting level by the recursive
/// helpers of `array.c`.
const FRAME_BYTES: u64 = 64;

/// The usable stack in bytes when a limit is configured.
fn stack_budget(vm: &Vm) -> Option<u64> {
    let max = u64::try_from(vm.ini_long("zend.max_allowed_stack_size")).ok()?;
    if max == 0 {
        return None;
    }
    // `zend.reserved_stack_size=0` (the default) is 48 KiB
    // (`OnUpdateReservedStackSize`: `ZEND_ALLOCA_MAX_SIZE + 16K`).
    let reserved = match vm.ini_long("zend.reserved_stack_size") {
        r if r <= 0 => 48 << 10,
        r => u64::try_from(r).unwrap_or(u64::MAX),
    };
    Some(max.saturating_sub(reserved).max(FRAME_BYTES))
}

/// The array nesting depth of `v` (0 for a non-array), following
/// references, ignoring arrays already on the current path (recursion is
/// reported by php-stdarr itself) and stopping once `cap` is exceeded.
#[must_use]
pub fn nesting_depth(v: &Value, cap: usize) -> usize {
    fn array_of(v: &Value) -> Option<ZArray> {
        match v {
            Value::Array(a) => Some(a.clone()),
            Value::Reference(r) => match r.try_borrow().as_deref() {
                Some(Value::Array(a)) => Some(a.clone()),
                _ => None,
            },
            _ => None,
        }
    }
    fn children(a: &ZArray) -> Vec<ZArray> {
        a.values().filter_map(array_of).collect()
    }
    let Some(root) = array_of(v) else {
        return 0;
    };
    // Depth-first walk: (array address, its array children, next child).
    let mut path = vec![(root.addr(), children(&root), 0usize)];
    let mut deepest = 1;
    while let Some((_, kids, next)) = path.last_mut() {
        let Some(child) = kids.get(*next).cloned() else {
            path.pop();
            continue;
        };
        *next += 1;
        if path.iter().any(|(addr, _, _)| *addr == child.addr()) {
            continue;
        }
        path.push((child.addr(), children(&child), 0));
        deepest = deepest.max(path.len());
        if deepest > cap {
            break;
        }
    }
    deepest
}

/// Throws PHP's stack-limit `Error` if recursing `depth` levels would
/// overflow the configured stack.
pub fn check_depth(vm: &mut Vm, depth: impl FnOnce(usize) -> usize) -> VmResult<()> {
    let Some(budget) = stack_budget(vm) else {
        return Ok(());
    };
    let allowed = usize::try_from(budget / FRAME_BYTES).unwrap_or(usize::MAX);
    if depth(allowed) <= allowed {
        return Ok(());
    }
    let msg = format!(
        "Maximum call stack size of {budget} bytes (zend.max_allowed_stack_size - zend.reserved_stack_size) reached. Infinite recursion?"
    );
    Err(vm.throw("Error", &msg))
}

#[cfg(test)]
mod tests {
    use super::nesting_depth;
    use php_runtime::{PhpRef, Value, ZArray};

    fn nest(levels: usize) -> Value {
        let mut v = Value::Array(ZArray::new());
        for _ in 1..levels {
            v = Value::Array(ZArray::from_values([v, Value::Int(1)]));
        }
        v
    }

    #[test]
    fn measures_depth_with_a_cap() {
        assert_eq!(nesting_depth(&Value::Int(1), 10), 0);
        assert_eq!(nesting_depth(&nest(1), 10), 1);
        assert_eq!(nesting_depth(&nest(5), 10), 5);
        assert_eq!(nesting_depth(&nest(50), 10), 11);
    }

    #[test]
    fn self_references_are_not_followed() {
        let r = PhpRef::new(Value::Array(ZArray::from_values([Value::Int(1)])));
        let with_self = match r.get() {
            Value::Array(mut a) => {
                a.make_mut().push(Value::Reference(r.clone())).ok();
                a
            }
            _ => unreachable!(),
        };
        r.set(Value::Array(with_self));
        assert!(nesting_depth(&Value::Reference(r.clone()), 100) <= 3);
        r.set(Value::Null);
    }
}
