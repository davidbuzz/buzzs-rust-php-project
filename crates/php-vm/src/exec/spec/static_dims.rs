// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Frame-local fast paths of the elements of a static property,
//! `self::$x[$k]...` (a generic `Fetch`, `Isset`/`Empty` or `Assign` of a
//! place whose base is `PlaceBase::StaticProp` with a literal name and a
//! class that is not dynamic): php-src's `ZEND_FETCH_STATIC_PROP_R`/`_IS`
//! /`_W` on a `CACHED_PTR` hit followed by the `ZEND_FETCH_DIM_*` chain,
//! `ZEND_ISSET_ISEMPTY_DIM_OBJ` or `ZEND_ASSIGN_DIM`.
//!
//! The property's cell comes from the place's run-time cache entry
//! (`rtcache::static_place_cell_in`, filled by the generic op, which
//! checks visibility and asymmetric visibility); each path completes the
//! op without a diagnostic or user code, or changes nothing (beyond a
//! separation the generic write repeats) and leaves it to the generic op.
//!
//! ref: Zend/zend_vm_def.h `ZEND_FETCH_STATIC_PROP_*`,
//! Zend/zend_execute.c `zend_fetch_static_property_address`.

use super::dims::{is_dims_value, plain_dims};
use super::{assign_dims, dims_value_of, release_place_str_tmps, write_keys_in};
use crate::opcode::{ClassSpec, Operand, PlaceBase, PlaceId, Seg};
use crate::vm::Frame;
use php_runtime::Value;

/// Whether place `pid` of frame `f` is `A::$name[d1]...[dn]`: a static
/// property named by a literal, of a class that is not dynamic, and only
/// dimensions after it.
fn is_static_dims(f: &Frame, pid: PlaceId) -> bool {
    f.code.places.get(pid as usize).is_some_and(|p| {
        matches!(
            &p.base,
            PlaceBase::StaticProp {
                class: ClassSpec::Named(_)
                    | ClassSpec::SelfClass
                    | ClassSpec::Parent
                    | ClassSpec::Static,
                name: Operand::Lit(_),
            }
        ) && !p.segs.is_empty()
            && p.segs.iter().all(|s| matches!(s, Seg::Dim(_)))
    })
}

/// A read of `A::$name[d1]...[dn]` (place `pid` of frame `f`) on the
/// property's cache hit: in `Read` mode only when every container holds
/// the offset ([`dims_value_of`]; a miss warns on the generic path), in
/// `Is` mode (`is`: `?? d`) also the silent null of a missing key or a
/// scalar container ([`is_dims_value`]). A typed property holding null
/// (possibly uninitialized) is left to the generic op. Consumed string
/// offsets are released. `None`, nothing changed, otherwise.
#[inline(never)]
pub(in crate::exec) fn fetch_static_dims_in(
    f: &mut Frame,
    pid: PlaceId,
    is: bool,
) -> Option<Value> {
    if !is_static_dims(f, pid) {
        return None;
    }
    let v = {
        let (cell, typed) = crate::rtcache::static_place_cell_in(f, pid)?;
        let cur = cell.try_borrow().ok()?;
        if typed && cur.is_null() {
            return None;
        }
        let segs = f.code.places.get(pid as usize)?.segs.as_slice();
        if is {
            if !plain_dims(f, segs) {
                return None;
            }
            is_dims_value(f, Some(&*cur), segs)?
        } else {
            dims_value_of(f, &cur, segs)?
        }
    };
    release_place_str_tmps(f, pid);
    Some(v)
}

/// `isset()` (`empty` false) or `empty()` of `A::$name[d1]...[dn]` on the
/// property's cache hit, as the `Is`-mode read of [`fetch_static_dims_in`]
/// (a missing key or a scalar container is not set; a string offset,
/// `ArrayAccess` or an object whose truth its class decides declines).
#[inline(never)]
pub(in crate::exec) fn isset_static_dims_in(
    f: &mut Frame,
    pid: PlaceId,
    empty: bool,
) -> Option<bool> {
    if !is_static_dims(f, pid) {
        return None;
    }
    let r = {
        let (cell, typed) = crate::rtcache::static_place_cell_in(f, pid)?;
        let cur = cell.try_borrow().ok()?;
        if typed && cur.is_null() {
            return None;
        }
        let segs = f.code.places.get(pid as usize)?.segs.as_slice();
        if !plain_dims(f, segs) {
            return None;
        }
        let v = is_dims_value(f, Some(&*cur), segs)?;
        if empty {
            !super::plain_truth(&v)?
        } else {
            !v.is_null()
        }
    };
    release_place_str_tmps(f, pid);
    Some(r)
}

/// `A::$name[d1]...[dn] = v` (place `pid` of frame `f`, at most four
/// offsets, each an int or a string) on the property's cache hit, when
/// the cell holds an array directly (not a reference: `$r =
/// &self::$x` keeps its own walk): `SEPARATE_ARRAY` and the
/// `FETCH_DIM_W` chain of [`assign_dims`] (a missing or null container
/// becomes an array, silently), then the store. The old element is
/// released after the cell's borrow ends (its destructor may read the
/// property). `Ok` with the assigned value; `Err(v)` back, nothing
/// changed beyond separations the generic write repeats, otherwise.
#[inline(never)]
pub(in crate::exec) fn assign_static_dims_in(
    f: &mut Frame,
    pid: PlaceId,
    v: Value,
) -> Result<Value, Value> {
    if !is_static_dims(f, pid) {
        return Err(v);
    }
    let Some((keys, n)) = write_keys_in(f, pid) else {
        return Err(v);
    };
    let Some((cell, _)) = crate::rtcache::static_place_cell_in(f, pid) else {
        return Err(v);
    };
    let ret = v.clone();
    let old = {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return Err(v);
        };
        let Value::Array(arr) = &mut *slot else {
            return Err(v);
        };
        match assign_dims(arr.make_mut(), &keys[..n], v) {
            Ok(old) => old,
            Err(v) => return Err(v),
        }
    };
    drop(cell);
    super::super::release_rooted(Some(old));
    release_place_str_tmps(f, pid);
    Ok(ret)
}
