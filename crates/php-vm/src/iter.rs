//! `foreach` (`FE_RESET_*`, `FE_FETCH_*`, `FE_FREE`) and generic iteration
//! of `Traversable` objects.

use crate::func::flags;
use crate::object;
use crate::opcode::{ForeachKind, Operand, PlaceId};
use crate::props::PropLookup;
use crate::vm::{ForeachIter, Vm, VmResult};
use php_runtime::{ArrayKey, Obj, PhpRef, Value, ZArray, ZStr};

impl Vm {
    /// Resolves `IteratorAggregate::getIterator()` chains.
    pub(crate) fn get_iterator(&mut self, o: &Obj) -> VmResult<Obj> {
        let mut cur = o.clone();
        for _ in 0..64 {
            if self.instance_of(&cur, b"iterator") {
                return Ok(cur);
            }
            if self.instance_of(&cur, b"iteratoraggregate") {
                let r = self.call_method_name(&cur, "getIterator", Vec::new())?;
                match r {
                    Value::Object(n) if self.instance_of(&n, b"traversable") => {
                        cur = n;
                        continue;
                    }
                    _ => {
                        return Err(self.make_throw(
                            "Exception",
                            &format!(
                                "Objects returned by {}::getIterator() must be traversable or implement interface Iterator",
                                object::display_name(&object::class_of(&cur).name)
                            ),
                        ))
                    }
                }
            }
            return Ok(cur);
        }
        Ok(cur)
    }

    /// Visible property names of an object for iteration from the current
    /// scope: (key value, unmangled name, declaring class for privates).
    pub(crate) fn visible_props(&mut self, o: &Obj) -> VmResult<Vec<(Value, ZStr, Option<ZStr>)>> {
        let target = self.lazy_object_for(o, None)?;
        let o = &target;
        let scope = self.scope();
        let cls = object::class_of(o).clone();
        let mut out = Vec::new();
        let mut table = Vec::new();
        for pi in &cls.props {
            let readable_hook = pi.hooks.as_ref().is_some_and(|h| h.get.is_some());
            let value = if pi.is_virtual() {
                None
            } else {
                object::data(o)
                    .slots
                    .borrow()
                    .get(pi.slot as usize)
                    .cloned()
                    .flatten()
            };
            if readable_hook || value.is_some() {
                table.push((ArrayKey::Str(pi.name.clone()), value, Some(pi.clone())));
            }
        }
        table.extend(
            self.object_prop_table(o, false)
                .into_iter()
                .filter(|(_, _, pi)| pi.is_none()),
        );
        if let Some(order) = object::prop_order(object::data(o)) {
            order.sort(&mut table, |(k, _, pi)| (pi.as_ref().map(|p| p.slot), k));
        }
        for (k, _, pi) in table {
            if let Some(pi) = pi {
                let name = pi.name.clone();
                match self.lookup_prop(&cls, &name, scope.as_ref()) {
                    PropLookup::Found(found) if found.slot == pi.slot => {
                        // A parent's private property seen from the
                        // parent's scope is not in the object's own
                        // property map: record its declaring class so
                        // the fetch does not look it up there.
                        let private_of = (pi.visibility() == flags::PRIVATE
                            && !pi.class.eq_ignore_ascii_case(cls.name.as_bytes()))
                        .then(|| pi.class.clone());
                        out.push((Value::Str(name.clone()), name, private_of));
                    }
                    _ => {
                        // Private props of other classes / inaccessible.
                        if pi.visibility() == flags::PRIVATE
                            && scope
                                .as_ref()
                                .is_some_and(|s| s.name.eq_ignore_ascii_case(pi.class.as_bytes()))
                        {
                            out.push((Value::Str(name.clone()), name, Some(pi.class.clone())));
                        }
                    }
                }
            } else {
                let name = match &k {
                    ArrayKey::Str(s) => s.clone(),
                    ArrayKey::Int(i) => ZStr::from(i.to_string()),
                };
                if name.as_bytes().first() == Some(&0) {
                    // `zend_fe_fetch_object_helper`: a dynamic key that
                    // looks mangled (`(object)["\0A\0b" => 1]`) is
                    // always accessible and yields its unmangled name.
                    out.push((unmangled_key(&name), name, None));
                } else if !matches!(
                    self.lookup_prop(&cls, &name, scope.as_ref()),
                    PropLookup::Found(pi) if pi.visibility() == flags::PRIVATE
                ) {
                    out.push((k.to_value(), name, None));
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn op_fe_reset(
        &mut self,
        subject: Operand,
        place: Option<PlaceId>,
        kind: ForeachKind,
        iter: u32,
    ) -> VmResult<bool> {
        let it = match kind {
            ForeachKind::Value => {
                let v = self.read(subject)?;
                match v {
                    Value::Array(a) => {
                        if a.is_empty() {
                            return Ok(false);
                        }
                        ForeachIter::Array { arr: a, pos: 0 }
                    }
                    Value::Object(o) => self.object_iter(o, false)?,
                    other => {
                        self.warning(&format!(
                            "foreach() argument must be of type array|object, {} given",
                            other.value_name()
                        ))?;
                        return Ok(false);
                    }
                }
            }
            ForeachKind::Ref => {
                let pid = place.expect("foreach by-ref place");
                let r = self.place_make_ref(pid)?;
                let cur = r.get();
                match cur {
                    Value::Array(a) => {
                        if a.is_empty() {
                            return Ok(false);
                        }
                        drop(a);
                        let id = {
                            let mut b = r.borrow_mut();
                            match &mut *b {
                                Value::Array(a) => a.make_mut().iterator_add(0),
                                _ => return Ok(false),
                            }
                        };
                        ForeachIter::ArrayRef { r, id }
                    }
                    Value::Object(o) => {
                        let mut it = self.object_iter(o, true)?;
                        if let ForeachIter::Props { subject, .. } = &mut it {
                            *subject = Value::Reference(r);
                        }
                        it
                    }
                    other => {
                        self.warning(&format!(
                            "foreach() argument must be of type array|object, {} given",
                            other.value_name()
                        ))?;
                        return Ok(false);
                    }
                }
            }
        };
        if let ForeachIter::Empty = it {
            return Ok(false);
        }
        self.frame_mut().iters[iter as usize] = Some(it);
        Ok(true)
    }

    /// `FE_RESET_R` / `FE_RESET_RW` on an object: `ce->get_iterator()`
    /// (`zend_fe_reset_iterator`) for a `Traversable`, its properties
    /// table otherwise.
    fn object_iter(&mut self, o: Obj, by_ref: bool) -> VmResult<ForeachIter> {
        // `ce->get_iterator` is only set for `Traversable` classes; other
        // classes with extension hooks (`SplFileInfo`) iterate properties.
        let traversable = self.instance_of(&o, b"traversable");
        if let Some(f) = crate::ext::hooks_of(&o)
            .and_then(|h| h.get_iterator)
            .filter(|_| traversable && !object::has_internal::<crate::generator::GenData>(&o))
        {
            let it = f(self, &o, by_ref)?;
            // The iterator holds its handle from here on (`zend_iterator_init`
            // in the handler); if `rewind` throws, it is released before the
            // handle (`OBJ_RELEASE(&iter->std)` frees the iterator's data
            // first), as the variant's field order drops them.
            let mut fe = ForeachIter::Native(
                crate::vm::NativeIter(it),
                false,
                object::HandleGuard::reserve(),
            );
            if let ForeachIter::Native(it, ..) = &mut fe {
                it.0.rewind(self)?;
            }
            return Ok(fe);
        }
        if object::has_internal::<crate::generator::GenData>(&o) {
            return self.generator_foreach(o, by_ref);
        }
        if traversable {
            return self.traversable_foreach(o, by_ref);
        }
        self.props_foreach(o, by_ref)
    }

    /// `zend_generator_get_iterator`: a `foreach` over the generator `gen`
    /// (the subject, or what `getIterator()` returned). The iterator takes
    /// its handle before the generator runs to its first `yield`.
    fn generator_foreach(&mut self, gen: Obj, by_ref: bool) -> VmResult<ForeachIter> {
        if self.gen_finished(&gen) {
            return Err(self.make_throw("Exception", "Cannot traverse an already closed generator"));
        }
        if by_ref
            && !object::internal::<crate::generator::GenData, _>(&gen, |d| {
                d.frame
                    .as_ref()
                    .and_then(|f| f.ufunc.as_ref())
                    .is_some_and(|u| u.flags & flags::RETURN_REF != 0)
            })
            .unwrap_or(false)
        {
            return Err(self.make_throw(
                "Exception",
                "You can only iterate a generator by-reference if it declared that it yields by-reference",
            ));
        }
        let fe = ForeachIter::Generator {
            obj: gen,
            started: false,
            handle: object::HandleGuard::reserve(),
        };
        if let ForeachIter::Generator { obj, .. } = &fe {
            self.gen_rewind(obj)?;
        }
        Ok(fe)
    }

    /// A `foreach` over a user `Traversable`: `zend_user_it_get_new_iterator`
    /// resolves `IteratorAggregate::getIterator()` chains, then the iterator
    /// takes its handle (`zend_user_it_get_iterator`) and rewinds.
    fn traversable_foreach(&mut self, o: Obj, by_ref: bool) -> VmResult<ForeachIter> {
        if by_ref {
            // `zend_user_it_get_new_iterator`: an IteratorAggregate
            // delegates to its iterator's own by-ref support (a
            // by-reference generator); user Iterators have none.
            if !self.instance_of(&o, b"iterator") && self.instance_of(&o, b"iteratoraggregate") {
                let r = self.call_method_name(&o, "getIterator", Vec::new())?;
                if let Value::Object(n) = r {
                    if self.instance_of(&n, b"traversable") {
                        return self.object_iter(n, true);
                    }
                }
                return Err(self.make_throw(
                    "Exception",
                    &format!(
                        "Objects returned by {}::getIterator() must be traversable or implement interface Iterator",
                        object::display_name(&object::class_of(&o).name)
                    ),
                ));
            }
            return Err(self.make_throw(
                "Error",
                "An iterator cannot be used with foreach by reference",
            ));
        }
        let it = self.get_iterator(&o)?;
        if object::has_internal::<crate::generator::GenData>(&it) {
            return self.generator_foreach(it, false);
        }
        let fe = ForeachIter::Iterator {
            obj: it,
            started: false,
            handle: object::HandleGuard::reserve(),
        };
        if let ForeachIter::Iterator { obj, .. } = &fe {
            self.call_method_name(obj, "rewind", Vec::new())?;
        }
        Ok(fe)
    }

    /// A `foreach` over an object's properties (`FE_RESET_R` / `FE_RESET_RW`
    /// without `get_iterator`): the loop walks `zobj->properties`, built
    /// here if it does not exist yet.
    fn props_foreach(&mut self, o: Obj, by_ref: bool) -> VmResult<ForeachIter> {
        // Iterating properties initializes a lazy object (a proxy iterates
        // its real instance).
        let o = self.lazy_object_for(&o, None)?;
        if !by_ref {
            if let Some(f) = object::class_of(&o)
                .hooks
                .as_ref()
                .and_then(|h| h.get_properties)
            {
                if let Some(t) = f(self, &o)? {
                    if t.is_empty() {
                        return Ok(ForeachIter::Empty);
                    }
                    return Ok(ForeachIter::Array { arr: t, pos: 0 });
                }
            }
        }
        // A table shared with a ghost's initialization snapshot is
        // separated first (`GC_REFCOUNT(properties) > 1`: `zend_array_dup`),
        // which freezes the declared properties' values in it; a table of
        // frozen copies is what the loop then walks.
        crate::lazy::separate_props_for_iteration(&o);
        if object::data(&o).flags.get() & object::oflags::STALE_PROPS != 0 {
            return self.frozen_props_foreach(&o, by_ref);
        }
        let keys = self.visible_props(&o)?;
        if keys.is_empty() {
            return Ok(ForeachIter::Empty);
        }
        Ok(ForeachIter::Props {
            target: std::rc::Rc::downgrade(o.rc()),
            subject: Value::Object(o),
            keys,
            pos: 0,
            by_ref,
        })
    }

    /// A `foreach` over an object whose properties table holds frozen copies
    /// of its declared properties ([`crate::lazy::stale_slots`]): php-src
    /// walks that table, so the loop sees the copies rather than the slots.
    /// Its plain entries pass `zend_check_property_access()` as dynamic
    /// ones do: a mangled key is accessible from any scope and yields the
    /// unmangled name, an unmangled one must not name a non-public declared
    /// property. By reference, the entries become references
    /// (`ZEND_FE_FETCH_RW`), so writes land in the table.
    #[cold]
    #[inline(never)]
    fn frozen_props_foreach(&mut self, o: &Obj, by_ref: bool) -> VmResult<ForeachIter> {
        let cls = object::class_of(o).clone();
        let scope = self.scope();
        let mut table = php_runtime::HashTable::new();
        for (key, value) in crate::lazy::property_table(o, by_ref) {
            let Some(value) = value else {
                continue;
            };
            let key = match key {
                ArrayKey::Str(name) if name.as_bytes().first() == Some(&0) => {
                    let (_, prop) = php_runtime::dump::unmangle_property_name(name.as_bytes());
                    ArrayKey::Str(ZStr::from(prop))
                }
                ArrayKey::Str(name) => match self.lookup_prop(&cls, &name, scope.as_ref()) {
                    PropLookup::NotFound => ArrayKey::Str(name),
                    PropLookup::Found(pi) if pi.visibility() == flags::PUBLIC => {
                        ArrayKey::Str(name)
                    }
                    PropLookup::Found(_) | PropLookup::Inaccessible(_) => continue,
                },
                key @ ArrayKey::Int(_) => key,
            };
            table.insert(key, value);
        }
        if table.is_empty() {
            return Ok(ForeachIter::Empty);
        }
        let mut arr = ZArray::from_table(table);
        if by_ref {
            let id = arr.make_mut().iterator_add(0);
            return Ok(ForeachIter::ArrayRef {
                r: PhpRef::new(Value::Array(arr)),
                id,
            });
        }
        Ok(ForeachIter::Array { arr, pos: 0 })
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn op_fe_fetch(&mut self, iter: u32, val: u32, key: Option<u32>) -> VmResult<bool> {
        let it = self.frame_mut().iters[iter as usize].take();
        let Some(mut it) = it else {
            return Ok(false);
        };
        let res = match &mut it {
            ForeachIter::Array { arr, pos } => {
                let p = arr.valid_pos(*pos);
                match arr.at(p) {
                    Some((k, v)) => {
                        let (k, v) = (k.to_value(), v.deref());
                        *pos = p + 1;
                        self.set_tmp(val, v);
                        if let Some(kt) = key {
                            self.set_tmp(kt, k);
                        }
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }
            ForeachIter::ArrayRef { r, id } => {
                let id = *id;
                // `ZEND_FE_FETCH_RW`: the iterated variable may have been
                // overwritten by the loop body with a non-array.
                let mut not_array = None;
                let item = {
                    let mut held = r.borrow_mut();
                    match &mut *held {
                        Value::Array(arr) => {
                            let table = arr.make_mut();
                            table.iterator_fetch(id).and_then(|pos| {
                                let key = table.at(pos).map(|(key, _)| key.to_value())?;
                                let slot = table.at_mut(pos)?;
                                Some((key, slot.make_ref()))
                            })
                        }
                        Value::Object(_) => None,
                        other => {
                            not_array = Some(other.type_name());
                            None
                        }
                    }
                };
                if let Some(ty) = not_array {
                    self.warning(&format!(
                        "foreach() argument must be of type array|object, {ty} given"
                    ))?;
                    return Ok(false);
                }
                match item {
                    Some((k, rf)) => {
                        self.set_tmp(val, Value::Reference(rf));
                        if let Some(kt) = key {
                            self.set_tmp(kt, k);
                        }
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }
            ForeachIter::Props {
                subject,
                target,
                keys,
                pos,
                by_ref,
            } => {
                let by_ref = *by_ref;
                let obj = match subject.deref() {
                    Value::Object(o) if by_ref => self.lazy_object_for(&o, None)?,
                    Value::Object(o) => o,
                    Value::Array(_) => return Ok(false),
                    other => {
                        self.warning(&format!(
                            "foreach() argument must be of type array|object, {} given",
                            other.value_name()
                        ))?;
                        return Ok(false);
                    }
                };
                let current = std::rc::Rc::downgrade(obj.rc());
                if !std::rc::Weak::ptr_eq(target, &current) {
                    *keys = self.visible_props(&obj)?;
                    *pos = 0;
                    *target = current;
                }
                if *pos >= keys.len() {
                    for (key, name, private_of) in self.visible_props(&obj)? {
                        if !keys.iter().any(|(_, old_name, old_private)| {
                            *old_name == name && *old_private == private_of
                        }) {
                            keys.push((key, name, private_of));
                        }
                    }
                }
                let mut found = None;
                while *pos < keys.len() {
                    let (k, name, private_of) = keys[*pos].clone();
                    *pos += 1;
                    // Skip properties removed since the reset.
                    let exists = match &private_of {
                        Some(_) => true,
                        None => Self::has_property_raw(&obj, &name),
                    };
                    if exists {
                        found = Some((k, name, private_of));
                        break;
                    }
                }
                match found {
                    Some((k, name, _)) => {
                        let v = if by_ref {
                            self.foreach_readonly_ref_check(&obj, &name)?;
                            let cls = object::class_of(&obj);
                            if let Some(pi) = cls.prop_map.get(&name) {
                                if pi
                                    .hooks
                                    .as_ref()
                                    .and_then(|h| h.get.as_ref())
                                    .is_some_and(|g| g.flags & flags::RETURN_REF == 0)
                                {
                                    return Err(self.make_throw(
                                        "Error",
                                        &format!(
                                            "Cannot create reference to property {}::${name}",
                                            object::display_name(&cls.name)
                                        ),
                                    ));
                                }
                            }
                            match nul_dynamic_prop_ref(&obj, &name) {
                                Some(r) => Value::Reference(r),
                                None => Value::Reference(self.property_ref(&obj, &name)?),
                            }
                        } else if name.as_bytes().first() == Some(&0) {
                            nul_dynamic_prop(&obj, &name)
                        } else {
                            self.read_property(&obj, &name, crate::opcode::FetchMode::Is)?
                        };
                        self.set_tmp(val, v);
                        if let Some(kt) = key {
                            self.set_tmp(kt, k);
                        }
                        Ok(true)
                    }
                    None => Ok(false),
                }
            }
            ForeachIter::Iterator { obj, started, .. } => {
                let obj = obj.clone();
                if *started {
                    self.call_method_name(&obj, "next", Vec::new())?;
                }
                *started = true;
                let valid = self.call_method_name(&obj, "valid", Vec::new())?;
                if self.to_bool(&valid)? {
                    let v = self.call_method_name(&obj, "current", Vec::new())?;
                    self.set_tmp(val, v);
                    if let Some(kt) = key {
                        let k = self.call_method_name(&obj, "key", Vec::new())?;
                        self.set_tmp(kt, k);
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            ForeachIter::Generator { obj, started, .. } => {
                let obj = obj.clone();
                if *started {
                    self.gen_next(&obj)?;
                }
                *started = true;
                if self.gen_valid(&obj)? {
                    let (k, v) = self.gen_current(&obj)?;
                    self.set_tmp(val, v);
                    if let Some(kt) = key {
                        self.set_tmp(kt, k);
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            ForeachIter::Native(it, started, _) => {
                if *started {
                    it.0.next(self)?;
                }
                *started = true;
                if it.0.valid(self)? {
                    let v = it.0.current(self)?;
                    self.set_tmp(val, v);
                    if let Some(kt) = key {
                        let k = it.0.key(self)?;
                        self.set_tmp(kt, k);
                    }
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            ForeachIter::Empty => Ok(false),
        };
        if let Some(slot) = self.frame_mut().iters.get_mut(iter as usize) {
            if slot.is_none() {
                *slot = Some(it);
            }
        }
        res
    }

    fn has_property_raw(o: &Obj, name: &ZStr) -> bool {
        let cls = object::class_of(o);
        if let Some(pi) = cls.prop_map.get(name) {
            if pi.flags & flags::STATIC == 0 {
                if pi.hooks.as_ref().is_some_and(|h| h.get.is_some()) {
                    return true;
                }
                return object::data(o)
                    .slots
                    .borrow()
                    .get(pi.slot as usize)
                    .is_some_and(Option::is_some);
            }
        }
        object::data(o)
            .dynamic
            .borrow()
            .as_ref()
            .is_some_and(|t| t.contains_key(&ArrayKey::Str(name.clone())))
    }

    pub(crate) fn op_fe_free(&mut self, iter: u32) {
        drop(self.fe_take(iter));
    }

    /// `FE_FREE` outside unwinding: the iterated object is released and its
    /// destructor runs before the `zend_object_iterator`'s own handle is
    /// released (`zend_objects_store_del` frees the inner object first), so
    /// handle numbers are reused in Zend's order.
    pub(crate) fn op_fe_free_and_destruct(&mut self, iter: u32) -> VmResult<()> {
        let handle = match self.fe_take(iter) {
            Some(
                ForeachIter::Iterator { obj, handle, .. }
                | ForeachIter::Generator { obj, handle, .. },
            ) => {
                drop(obj);
                handle
            }
            Some(ForeachIter::Native(it, _, handle)) => {
                drop(it);
                handle
            }
            _ => return Ok(()),
        };
        let r = if object::has_pending_destructors() {
            self.run_pending_destructors()
        } else {
            Ok(())
        };
        drop(handle);
        r
    }

    /// Takes iterator `iter` out of the frame, detaching a by-reference
    /// array iterator from its array. The reference a by-reference loop
    /// holds (`FE_RESET_RW` made the subject one) is released here the way
    /// `FE_FREE` does, with `zval_ptr_dtor_nogc`: no possible root is
    /// buffered for it.
    fn fe_take(&mut self, iter: u32) -> Option<ForeachIter> {
        let it = self
            .frame_mut()
            .iters
            .get_mut(iter as usize)
            .and_then(Option::take);
        match it {
            Some(ForeachIter::ArrayRef { r, id }) => {
                if r.try_borrow().is_some() {
                    let mut b = r.borrow_mut();
                    if let Value::Array(a) = &mut *b {
                        if a.has_iterators() {
                            a.make_mut().iterator_del(id);
                        }
                    }
                }
                crate::gc::release_duplicate(Value::Reference(r));
                None
            }
            Some(ForeachIter::Props {
                subject: subject @ Value::Reference(_),
                ..
            }) => {
                crate::gc::release_duplicate(subject);
                None
            }
            other => other,
        }
    }

    /// Collects the (key, value) pairs of an array or `Traversable`
    /// (`ZEND_SEND_UNPACK`, `ZEND_ADD_ARRAY_UNPACK`).
    pub(crate) fn iterate_to_vec(&mut self, v: &Value) -> VmResult<Vec<(Value, Value)>> {
        match v {
            Value::Array(a) => Ok(a.iter().map(|(k, v)| (k.to_value(), v.deref())).collect()),
            Value::Object(o) => self.object_to_vec(o),
            _ => Ok(Vec::new()),
        }
    }

    /// `ce->get_iterator()` on `o`, then a `rewind` / `valid` / `current` /
    /// `key` / `next` walk to the end. The iterator holds an object handle
    /// for the walk ([`UnpackIter`]), as `zend_iterator_init` gives one.
    fn object_to_vec(&mut self, o: &Obj) -> VmResult<Vec<(Value, Value)>> {
        if let Some(f) = crate::ext::hooks_of(o).and_then(|h| h.get_iterator) {
            let mut it = UnpackIter::new(f(self, o, false)?);
            let mut out = Vec::new();
            it.inner.rewind(self)?;
            while it.inner.valid(self)? {
                let value = it.inner.current(self)?;
                let key = it.inner.key(self)?;
                out.push((key, value));
                it.inner.next(self)?;
            }
            it.finish(self)?;
            return Ok(out);
        }
        if object::has_internal::<crate::generator::GenData>(o) {
            return self.generator_to_vec(o.clone());
        }
        if self.instance_of(o, b"traversable") {
            let it = self.get_iterator(o)?;
            if object::has_internal::<crate::generator::GenData>(&it) {
                return self.generator_to_vec(it);
            }
            // `zend_user_it_get_iterator`: the handle comes after the
            // `getIterator()` calls, before `rewind`.
            let it = UnpackIter::new(it);
            let mut out = Vec::new();
            self.call_method_name(&it.inner, "rewind", Vec::new())?;
            loop {
                let valid = self.call_method_name(&it.inner, "valid", Vec::new())?;
                if !self.to_bool(&valid)? {
                    break;
                }
                let value = self.call_method_name(&it.inner, "current", Vec::new())?;
                let key = self.call_method_name(&it.inner, "key", Vec::new())?;
                out.push((key, value));
                self.call_method_name(&it.inner, "next", Vec::new())?;
            }
            it.finish(self)?;
            return Ok(out);
        }
        // Not traversable (the bare VM's `iterator_to_array()`): the
        // visible properties.
        let mut out = Vec::new();
        for (key, name, _) in self.visible_props(o)? {
            let value = if name.as_bytes().first() == Some(&0) {
                nul_dynamic_prop(o, &name)
            } else {
                self.read_property(o, &name, crate::opcode::FetchMode::Is)?
            };
            out.push((key, value));
        }
        Ok(out)
    }

    /// `zend_generator_get_iterator` and the walk over the generator `gen`
    /// (unpacked directly or returned by `getIterator()`): the handle is
    /// taken before the generator runs to its first `yield`.
    fn generator_to_vec(&mut self, gen: Obj) -> VmResult<Vec<(Value, Value)>> {
        if self.gen_finished(&gen) {
            return Err(self.make_throw("Exception", "Cannot traverse an already closed generator"));
        }
        let it = UnpackIter::new(gen);
        let mut out = Vec::new();
        self.gen_rewind(&it.inner)?;
        while self.gen_valid(&it.inner)? {
            out.push(self.gen_current(&it.inner)?);
            self.gen_next(&it.inner)?;
        }
        it.finish(self)?;
        Ok(out)
    }

    /// Converts iteration results to an array (`iterator_to_array`).
    pub(crate) fn pairs_to_array(
        &mut self,
        pairs: Vec<(Value, Value)>,
        preserve_keys: bool,
    ) -> VmResult<ZArray> {
        let mut t = php_runtime::HashTable::new();
        for (k, v) in pairs {
            if preserve_keys {
                let key = self.array_key_for_insert(&k)?;
                t.insert(key, v);
            } else {
                let _ = t.push(v);
            }
        }
        Ok(ZArray::from_table(t))
    }

    /// Converts a value to an array key for insertion (with PHP's errors).
    pub(crate) fn array_key_for_insert(&mut self, k: &Value) -> VmResult<ArrayKey> {
        match self.array_key(k, crate::exec::KeyCtx::Write)? {
            Some(k) => Ok(k),
            None => Ok(ArrayKey::Str(ZStr::empty())),
        }
    }

    /// Converts an offset to an array key; `None` after an error was thrown.
    pub(crate) fn array_key(
        &mut self,
        k: &Value,
        ctx: crate::exec::KeyCtx,
    ) -> VmResult<Option<ArrayKey>> {
        let mut diags = Vec::new();
        let r = crate::place::key_of(k, &mut diags, ctx);
        self.flush_diags(diags)?;
        match r {
            Ok(k) => Ok(Some(k)),
            Err(e) => Err(self.walk_err(e)),
        }
    }
}

/// A `zend_object_iterator` driven to its end outside `foreach`
/// (`ZEND_SEND_UNPACK`, `ZEND_ADD_ARRAY_UNPACK`): the iterated object (or
/// the native iterator) and the object handle `zend_iterator_init` gives the
/// iterator, taken after any `getIterator()` call and before `rewind`. The
/// fields drop in order: `zend_iterator_dtor` releases the object
/// (`zend_user_it_dtor`) before the iterator's own handle returns to the
/// free list (`zend_objects_store_del` of `iter->std`).
struct UnpackIter<T> {
    inner: T,
    handle: object::HandleGuard,
}

impl<T> UnpackIter<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            handle: object::HandleGuard::reserve(),
        }
    }

    /// `zend_iterator_dtor` after the walk: the object goes first (a
    /// destructor its release queues runs now, as it would inside
    /// `zend_objects_store_del`), then the handle. The caller releases the
    /// unpacked operand afterwards (`FREE_OP1`).
    fn finish(self, vm: &mut Vm) -> VmResult<()> {
        let Self { inner, handle } = self;
        drop(inner);
        let r = if object::has_pending_destructors() {
            vm.run_pending_destructors()
        } else {
            Ok(())
        };
        drop(handle);
        r
    }
}

/// The iteration key of a NUL-prefixed dynamic property: the property part
/// of `zend_unmangle_property_name_ex()`.
#[cold]
fn unmangled_key(name: &ZStr) -> Value {
    let (_, prop) = php_runtime::dump::unmangle_property_name(name.as_bytes());
    Value::Str(ZStr::from(prop))
}

/// Reads a NUL-prefixed dynamic property straight from the property table,
/// as `zend_fe_fetch_object_helper` does (a property fetch would reject the
/// name).
#[cold]
fn nul_dynamic_prop(o: &Obj, name: &ZStr) -> Value {
    object::data(o)
        .dynamic
        .borrow()
        .as_ref()
        .and_then(|t| t.get(&ArrayKey::Str(name.clone())))
        .map_or(Value::Null, Value::deref)
}

/// By-reference counterpart of [`nul_dynamic_prop`] (`ZEND_FE_FETCH_RW`);
/// `None` for an ordinary name.
fn nul_dynamic_prop_ref(o: &Obj, name: &ZStr) -> Option<PhpRef> {
    if name.as_bytes().first() != Some(&0) {
        return None;
    }
    object::data(o)
        .dynamic
        .borrow_mut()
        .as_mut()
        .and_then(|t| t.get_mut(&ArrayKey::Str(name.clone())))
        .map(Value::make_ref)
}
