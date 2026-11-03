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
    pub(crate) fn visible_props(&mut self, o: &Obj) -> Vec<(Value, ZStr, Option<ZStr>)> {
        let target = self.lazy_object_for(o, None).unwrap_or_else(|_| o.clone());
        let o = &target;
        let scope = self.scope();
        let cls = object::class_of(o).clone();
        let mut out = Vec::new();
        for (k, v, pi) in self.object_prop_table(o, false) {
            if v.is_none() {
                continue;
            }
            match pi {
                Some(pi) => {
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
                                && scope.as_ref().is_some_and(|s| {
                                    s.name.eq_ignore_ascii_case(pi.class.as_bytes())
                                })
                            {
                                out.push((Value::Str(name.clone()), name, Some(pi.class.clone())));
                            }
                        }
                    }
                }
                None => {
                    let name = match &k {
                        ArrayKey::Str(s) => s.clone(),
                        ArrayKey::Int(i) => ZStr::from(i.to_string()),
                    };
                    out.push((k.to_value(), name, None));
                }
            }
        }
        out
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
                            other.type_name()
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
                    Value::Object(o) => self.object_iter(o, true)?,
                    other => {
                        self.warning(&format!(
                            "foreach() argument must be of type array|object, {} given",
                            other.type_name()
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

    fn object_iter(&mut self, o: Obj, by_ref: bool) -> VmResult<ForeachIter> {
        if let Some(f) = crate::ext::hooks_of(&o)
            .and_then(|h| h.get_iterator)
            .filter(|_| !object::has_internal::<crate::generator::GenData>(&o))
        {
            let mut it = f(self, &o, by_ref)?;
            it.rewind(self)?;
            return Ok(ForeachIter::Native(crate::vm::NativeIter(it), false));
        }
        if object::has_internal::<crate::generator::GenData>(&o) {
            if by_ref
                && !object::internal::<crate::generator::GenData, _>(&o, |d| {
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
            if self.gen_finished(&o) {
                return Err(
                    self.make_throw("Exception", "Cannot traverse an already closed generator")
                );
            }
            self.gen_rewind(&o)?;
            return Ok(ForeachIter::Generator {
                obj: o,
                started: false,
            });
        }
        if self.instance_of(&o, b"traversable") {
            if by_ref {
                // `zend_user_it_get_new_iterator`: an IteratorAggregate
                // delegates to its iterator's own by-ref support (a
                // by-reference generator); user Iterators have none.
                if !self.instance_of(&o, b"iterator") && self.instance_of(&o, b"iteratoraggregate")
                {
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
                if self.gen_finished(&it) {
                    return Err(
                        self.make_throw("Exception", "Cannot traverse an already closed generator")
                    );
                }
                self.gen_rewind(&it)?;
                return Ok(ForeachIter::Generator {
                    obj: it,
                    started: false,
                });
            }
            self.call_method_name(&it, "rewind", Vec::new())?;
            return Ok(ForeachIter::Iterator {
                obj: it,
                started: false,
            });
        }
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
        let keys = self.visible_props(&o);
        if keys.is_empty() {
            return Ok(ForeachIter::Empty);
        }
        Ok(ForeachIter::Props {
            obj: o,
            keys,
            pos: 0,
            by_ref,
        })
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
                let item = {
                    let mut b = r.borrow_mut();
                    match &mut *b {
                        Value::Array(a) => {
                            let t = a.make_mut();
                            match t.iterator_fetch(id) {
                                Some(p) => {
                                    let k = t.at(p).map(|(k, _)| k.to_value());
                                    let slot = t.at_mut(p);
                                    match (k, slot) {
                                        (Some(k), Some(slot)) => Some((k, slot.make_ref())),
                                        _ => None,
                                    }
                                }
                                None => None,
                            }
                        }
                        _ => None,
                    }
                };
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
                obj,
                keys,
                pos,
                by_ref,
            } => {
                let obj = obj.clone();
                let by_ref = *by_ref;
                let mut found = None;
                while *pos < keys.len() {
                    let (k, name, private_of) = keys[*pos].clone();
                    *pos += 1;
                    // Skip properties removed since the reset.
                    let exists = match &private_of {
                        Some(_) => true,
                        None => self.has_property_raw(&obj, &name),
                    };
                    if exists {
                        found = Some((k, name, private_of));
                        break;
                    }
                }
                match found {
                    Some((k, name, _)) => {
                        let v = if by_ref {
                            Value::Reference(self.property_ref(&obj, &name)?)
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
            ForeachIter::Iterator { obj, started } => {
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
            ForeachIter::Generator { obj, started } => {
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
            ForeachIter::Native(it, started) => {
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

    fn has_property_raw(&self, o: &Obj, name: &ZStr) -> bool {
        let cls = object::class_of(o);
        if let Some(pi) = cls.prop_map.get(name) {
            if pi.flags & flags::STATIC == 0 {
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
        let it = self
            .frame_mut()
            .iters
            .get_mut(iter as usize)
            .and_then(Option::take);
        if let Some(ForeachIter::ArrayRef { r, id }) = it {
            if r.try_borrow().is_some() {
                let mut b = r.borrow_mut();
                if let Value::Array(a) = &mut *b {
                    if a.has_iterators() {
                        a.make_mut().iterator_del(id);
                    }
                }
            }
        }
    }

    /// Collects the (key, value) pairs of an array or `Traversable`.
    pub(crate) fn iterate_to_vec(&mut self, v: &Value) -> VmResult<Vec<(Value, Value)>> {
        match v {
            Value::Array(a) => Ok(a.iter().map(|(k, v)| (k.to_value(), v.deref())).collect()),
            Value::Object(o) => {
                let mut out = Vec::new();
                if let Some(f) = crate::ext::hooks_of(o).and_then(|h| h.get_iterator) {
                    let mut it = f(self, o, false)?;
                    it.rewind(self)?;
                    while it.valid(self)? {
                        let v = it.current(self)?;
                        let k = it.key(self)?;
                        out.push((k, v));
                        it.next(self)?;
                    }
                    return Ok(out);
                }
                if object::has_internal::<crate::generator::GenData>(o) {
                    if self.gen_finished(o) {
                        return Err(self.make_throw(
                            "Exception",
                            "Cannot traverse an already closed generator",
                        ));
                    }
                    self.gen_rewind(o)?;
                    while self.gen_valid(o)? {
                        out.push(self.gen_current(o)?);
                        self.gen_next(o)?;
                    }
                    return Ok(out);
                }
                if self.instance_of(o, b"traversable") {
                    let it = self.get_iterator(o)?;
                    if object::has_internal::<crate::generator::GenData>(&it) {
                        return self.iterate_to_vec(&Value::Object(it));
                    }
                    self.call_method_name(&it, "rewind", Vec::new())?;
                    loop {
                        let valid = self.call_method_name(&it, "valid", Vec::new())?;
                        if !self.to_bool(&valid)? {
                            break;
                        }
                        let v = self.call_method_name(&it, "current", Vec::new())?;
                        let k = self.call_method_name(&it, "key", Vec::new())?;
                        out.push((k, v));
                        self.call_method_name(&it, "next", Vec::new())?;
                    }
                    return Ok(out);
                }
                for (k, name, _) in self.visible_props(o) {
                    let v = self.read_property(o, &name, crate::opcode::FetchMode::Is)?;
                    out.push((k, v));
                }
                Ok(out)
            }
            _ => Ok(Vec::new()),
        }
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

    /// A reference to a value for by-ref iteration helpers.
    pub(crate) fn make_value_ref(v: Value) -> PhpRef {
        match v {
            Value::Reference(r) => r,
            o => PhpRef::new(o),
        }
    }
}
