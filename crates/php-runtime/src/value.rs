//! The PHP value (`zval`) and its refcounted payload types.
//!
//! Copying a [`Value`] (`clone`) is always cheap: strings, arrays, objects,
//! resources and references are reference-counted handles. Arrays are
//! copy-on-write: mutating an array through [`crate::ZArray::make_mut`]
//! separates it from other holders, so `$b = $a; $b[] = 1;` leaves `$a`
//! untouched.
//!
//! PHP references (`&`) are modelled by [`PhpRef`], a shared mutable cell. A
//! variable or array element that is a reference holds `Value::Reference`.

use crate::array::ZArray;
use crate::string::ZStr;
use std::any::Any;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::fmt;
use std::rc::Rc;

/// A PHP value (`zval`).
#[derive(Clone, Default)]
pub enum Value {
    /// `null`.
    #[default]
    Null,
    /// `true` / `false`.
    Bool(bool),
    /// A 64-bit integer.
    Int(i64),
    /// A double-precision float.
    Float(f64),
    /// A binary string.
    Str(ZStr),
    /// An ordered hash table (copy-on-write).
    Array(ZArray),
    /// An object handle (identity semantics).
    Object(Obj),
    /// A resource handle.
    Resource(Resource),
    /// A PHP reference (`&`): a shared, mutable slot.
    Reference(PhpRef),
}

impl Value {
    /// Creates a string value.
    #[must_use]
    pub fn string(s: impl Into<ZStr>) -> Self {
        Value::Str(s.into())
    }

    /// Creates an empty array value.
    #[must_use]
    pub fn empty_array() -> Self {
        Value::Array(ZArray::new())
    }

    /// The type name used in error messages (`zend_zval_type_name`): `null`,
    /// `bool`, `int`, `float`, `string`, `array`, `resource`, or the class name
    /// for objects. References are looked through.
    #[must_use]
    pub fn type_name(&self) -> String {
        match self {
            Value::Object(o) => o.class_name().to_string_lossy(),
            Value::Reference(r) => r.get().type_name(),
            other => other.simple_type_name().to_string(),
        }
    }

    /// Like [`Value::type_name`] but distinguishes `true`/`false`
    /// (`zend_zval_value_name`).
    #[must_use]
    pub fn value_name(&self) -> String {
        match self {
            Value::Bool(true) => "true".to_string(),
            Value::Bool(false) => "false".to_string(),
            Value::Reference(r) => r.get().value_name(),
            other => other.type_name(),
        }
    }

    /// The type name without class names: objects are `"object"`
    /// (`zend_get_type_by_const`).
    #[must_use]
    pub fn simple_type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Resource(_) => "resource",
            Value::Reference(r) => r.get().simple_type_name(),
        }
    }

    /// The legacy type name returned by `gettype()` (`"integer"`, `"double"`,
    /// `"boolean"`, `"NULL"`, `"resource (closed)"`, ...).
    #[must_use]
    pub fn legacy_type_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Float(_) => "double",
            Value::Str(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
            Value::Resource(r) => {
                if r.type_name().is_some() {
                    "resource"
                } else {
                    "resource (closed)"
                }
            }
            Value::Reference(r) => r.get().legacy_type_name(),
        }
    }

    /// Whether the value is `null` (references are looked through).
    #[must_use]
    pub fn is_null(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Reference(r) => r.borrow().is_null(),
            _ => false,
        }
    }

    /// Whether this value is a [`Value::Reference`].
    #[must_use]
    pub fn is_reference(&self) -> bool {
        matches!(self, Value::Reference(_))
    }

    /// Returns the dereferenced value (a cheap clone of the referenced value
    /// for references, a clone of `self` otherwise).
    #[must_use]
    pub fn deref(&self) -> Value {
        match self {
            Value::Reference(r) => r.get(),
            other => other.clone(),
        }
    }

    /// Unwraps a reference in place (`ZVAL_DEREF` + copy): afterwards `self`
    /// is never a `Value::Reference`.
    pub fn unwrap_reference(&mut self) {
        if let Value::Reference(r) = self {
            let inner = r.get();
            *self = inner;
        }
    }

    /// Turns `self` into a reference (`ZVAL_MAKE_REF`) and returns it. If it is
    /// already a reference, returns a handle to the existing one.
    pub fn make_ref(&mut self) -> PhpRef {
        if let Value::Reference(r) = self {
            return r.clone();
        }
        let r = PhpRef::new(std::mem::take(self));
        *self = Value::Reference(r.clone());
        r
    }

    /// Returns the array if this is an array (not looking through references).
    #[must_use]
    pub fn as_array(&self) -> Option<&ZArray> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns the string if this is a string (not looking through references).
    #[must_use]
    pub fn as_str(&self) -> Option<&ZStr> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the integer if this is an int.
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Returns the float if this is a float.
    #[must_use]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Returns the object if this is an object.
    #[must_use]
    pub fn as_object(&self) -> Option<&Obj> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Returns a mutable array, separating it (copy-on-write) if shared.
    pub fn as_array_mut(&mut self) -> Option<&mut crate::array::HashTable> {
        match self {
            Value::Array(a) => Some(a.make_mut()),
            _ => None,
        }
    }

    /// Whether the value is a scalar (null excluded): bool, int, float, string.
    #[must_use]
    pub fn is_scalar(&self) -> bool {
        matches!(
            self,
            Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_)
        )
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => f.write_str("Null"),
            Value::Bool(b) => write!(f, "Bool({b})"),
            Value::Int(i) => write!(f, "Int({i})"),
            Value::Float(x) => write!(f, "Float({x:?})"),
            Value::Str(s) => write!(f, "Str({s:?})"),
            Value::Array(a) => write!(f, "Array({a:?})"),
            Value::Object(o) => write!(f, "{o:?}"),
            Value::Resource(r) => write!(f, "{r:?}"),
            Value::Reference(r) => match r.0.value.try_borrow() {
                Ok(v) => write!(f, "Reference({:?})", &*v),
                Err(_) => f.write_str("Reference(<borrowed>)"),
            },
        }
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Int(i)
    }
}

impl From<i32> for Value {
    fn from(i: i32) -> Self {
        Value::Int(i64::from(i))
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}

impl From<ZStr> for Value {
    fn from(s: ZStr) -> Self {
        Value::Str(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(ZStr::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(ZStr::from(s))
    }
}

impl From<&[u8]> for Value {
    fn from(s: &[u8]) -> Self {
        Value::Str(ZStr::from(s))
    }
}

impl From<ZArray> for Value {
    fn from(a: ZArray) -> Self {
        Value::Array(a)
    }
}

impl From<Obj> for Value {
    fn from(o: Obj) -> Self {
        Value::Object(o)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(o: Option<T>) -> Self {
        o.map_or(Value::Null, Into::into)
    }
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

struct RefInner {
    value: RefCell<Value>,
    /// Opaque slot for the VM (e.g. typed-property type sources,
    /// `ZEND_REF_HAS_TYPE_SOURCES`).
    meta: RefCell<Option<Rc<dyn Any>>>,
}

/// A PHP reference (`zend_reference`): a shared mutable slot holding a value.
///
/// Cloning a `PhpRef` creates another handle to the *same* slot (it is how
/// `$a = &$b` is represented); [`PhpRef::refcount`] reports the number of
/// handles, which `var_dump` uses to decide whether to print `&`.
#[derive(Clone)]
pub struct PhpRef(Rc<RefInner>);

impl PhpRef {
    /// Creates a new reference holding `value`. A reference never directly
    /// contains another reference: nested references are unwrapped.
    #[must_use]
    pub fn new(value: Value) -> Self {
        let value = match value {
            Value::Reference(r) => r.get(),
            v => v,
        };
        PhpRef(Rc::new(RefInner {
            value: RefCell::new(value),
            meta: RefCell::new(None),
        }))
    }

    /// Returns a clone of the referenced value.
    #[must_use]
    pub fn get(&self) -> Value {
        self.0.value.borrow().clone()
    }

    /// Replaces the referenced value, returning the old one.
    pub fn set(&self, value: Value) -> Value {
        let value = match value {
            Value::Reference(r) if !PhpRef::ptr_eq(&r, self) => r.get(),
            Value::Reference(_) => return self.get(),
            v => v,
        };
        std::mem::replace(&mut *self.0.value.borrow_mut(), value)
    }

    /// Borrows the referenced value.
    ///
    /// # Panics
    /// Panics if the value is currently mutably borrowed.
    #[must_use]
    pub fn borrow(&self) -> Ref<'_, Value> {
        self.0.value.borrow()
    }

    /// Mutably borrows the referenced value.
    ///
    /// # Panics
    /// Panics if the value is currently borrowed.
    #[must_use]
    pub fn borrow_mut(&self) -> RefMut<'_, Value> {
        self.0.value.borrow_mut()
    }

    /// Borrows the referenced value unless it is currently mutably borrowed.
    #[must_use]
    pub fn try_borrow(&self) -> Option<Ref<'_, Value>> {
        self.0.value.try_borrow().ok()
    }

    /// Mutably borrows the referenced value unless it is currently borrowed.
    #[must_use]
    pub fn try_borrow_mut(&self) -> Option<RefMut<'_, Value>> {
        self.0.value.try_borrow_mut().ok()
    }

    /// Whether two handles denote the same reference.
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    /// Number of handles to this reference (`GC_REFCOUNT`).
    #[must_use]
    pub fn refcount(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    /// An address usable as an identity key (for recursion detection and
    /// serialization back-references).
    #[must_use]
    pub fn addr(&self) -> usize {
        Rc::as_ptr(&self.0).cast::<u8>() as usize
    }

    /// The VM's metadata attached to this reference, if any.
    #[must_use]
    pub fn meta(&self) -> Option<Rc<dyn Any>> {
        self.0.meta.borrow().clone()
    }

    /// Attaches (or clears) VM metadata.
    pub fn set_meta(&self, meta: Option<Rc<dyn Any>>) {
        *self.0.meta.borrow_mut() = meta;
    }
}

impl fmt::Debug for PhpRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhpRef({:?})", self.get())
    }
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

/// The interface the VM implements for its object representation.
///
/// The runtime only needs an object's identity (its handle number, as printed
/// by `var_dump` as `#N`), its class name and a way for the VM to downcast.
/// Everything else (properties, casts, comparison) goes through
/// [`crate::RuntimeContext`].
pub trait PhpObject: Any {
    /// The object handle (`Z_OBJ_HANDLE`), unique among live objects.
    fn handle(&self) -> u32;
    /// The class name (`get_class_name` handler).
    fn class_name(&self) -> ZStr;
    /// Upcast for downcasting to the VM's concrete type.
    fn as_any(&self) -> &dyn Any;
}

/// A handle to a PHP object. Clones share the object; equality is identity.
#[derive(Clone)]
pub struct Obj(Rc<dyn PhpObject>);

impl Obj {
    /// Wraps a VM object.
    pub fn new<T: PhpObject>(object: T) -> Self {
        Obj(Rc::new(object))
    }

    /// Wraps an already reference-counted VM object.
    #[must_use]
    pub fn from_rc(rc: Rc<dyn PhpObject>) -> Self {
        Obj(rc)
    }

    /// The underlying reference-counted object.
    #[must_use]
    pub fn rc(&self) -> &Rc<dyn PhpObject> {
        &self.0
    }

    /// The object handle number.
    #[must_use]
    pub fn handle(&self) -> u32 {
        self.0.handle()
    }

    /// The class name.
    #[must_use]
    pub fn class_name(&self) -> ZStr {
        self.0.class_name()
    }

    /// Downcasts to the VM's concrete object type.
    #[must_use]
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref::<T>()
    }

    /// Whether two handles denote the same object (`===`).
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }

    /// Number of handles to the object (`GC_REFCOUNT`).
    #[must_use]
    pub fn refcount(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    /// An address usable as an identity key.
    #[must_use]
    pub fn addr(&self) -> usize {
        Rc::as_ptr(&self.0).cast::<u8>() as usize
    }
}

impl PartialEq for Obj {
    fn eq(&self, other: &Self) -> bool {
        Obj::ptr_eq(self, other)
    }
}

impl fmt::Debug for Obj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Object({}#{})", self.class_name(), self.handle())
    }
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

struct ResourceInner {
    handle: i64,
    type_name: RefCell<Option<ZStr>>,
    data: RefCell<Option<Box<dyn Any>>>,
}

/// A PHP resource (`zend_resource`): a numbered handle with a type name and
/// an opaque payload owned by the extension that created it.
#[derive(Clone)]
pub struct Resource(Rc<ResourceInner>);

thread_local! {
    static NEXT_RESOURCE_ID: Cell<i64> = const { Cell::new(1) };
}

impl Resource {
    /// Creates a resource with an explicit handle number.
    #[must_use]
    pub fn with_handle(
        handle: i64,
        type_name: impl Into<ZStr>,
        data: Option<Box<dyn Any>>,
    ) -> Self {
        Resource(Rc::new(ResourceInner {
            handle,
            type_name: RefCell::new(Some(type_name.into())),
            data: RefCell::new(data),
        }))
    }

    /// Creates a resource with the next free handle number of this thread.
    #[must_use]
    pub fn new(type_name: impl Into<ZStr>, data: Option<Box<dyn Any>>) -> Self {
        let handle = NEXT_RESOURCE_ID.with(|c| {
            let h = c.get();
            c.set(h + 1);
            h
        });
        Self::with_handle(handle, type_name, data)
    }

    /// Sets the next handle number allocated by [`Resource::new`].
    pub fn set_next_handle(handle: i64) {
        NEXT_RESOURCE_ID.with(|c| c.set(handle));
    }

    /// The resource handle number (`Resource id #N`).
    #[must_use]
    pub fn handle(&self) -> i64 {
        self.0.handle
    }

    /// The type name, or `None` once closed (printed as `Unknown`).
    #[must_use]
    pub fn type_name(&self) -> Option<ZStr> {
        self.0.type_name.borrow().clone()
    }

    /// Closes the resource: drops the payload and forgets the type.
    pub fn close(&self) -> Option<Box<dyn Any>> {
        *self.0.type_name.borrow_mut() = None;
        self.0.data.borrow_mut().take()
    }

    /// Whether the resource has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.0.type_name.borrow().is_none()
    }

    /// Borrows the payload.
    #[must_use]
    pub fn data(&self) -> Ref<'_, Option<Box<dyn Any>>> {
        self.0.data.borrow()
    }

    /// Mutably borrows the payload.
    #[must_use]
    pub fn data_mut(&self) -> RefMut<'_, Option<Box<dyn Any>>> {
        self.0.data.borrow_mut()
    }

    /// Whether two handles denote the same resource.
    #[must_use]
    pub fn ptr_eq(a: &Self, b: &Self) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }
}

impl fmt::Debug for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Resource(#{} {})",
            self.handle(),
            self.type_name()
                .map_or_else(|| "Unknown".to_string(), |t| t.to_string_lossy())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestObj(u32);
    impl PhpObject for TestObj {
        fn handle(&self) -> u32 {
            self.0
        }
        fn class_name(&self) -> ZStr {
            ZStr::from("Foo")
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn type_names() {
        assert_eq!(Value::Null.type_name(), "null");
        assert_eq!(Value::Bool(true).type_name(), "bool");
        assert_eq!(Value::Bool(true).value_name(), "true");
        assert_eq!(Value::Int(1).type_name(), "int");
        assert_eq!(Value::Float(1.0).type_name(), "float");
        assert_eq!(Value::from("x").type_name(), "string");
        assert_eq!(Value::empty_array().type_name(), "array");
        let o = Obj::new(TestObj(3));
        assert_eq!(Value::Object(o.clone()).type_name(), "Foo");
        assert_eq!(Value::Object(o).simple_type_name(), "object");
        assert_eq!(Value::Int(1).legacy_type_name(), "integer");
    }

    #[test]
    fn references_share() {
        let mut a = Value::Int(1);
        let r = a.make_ref();
        assert!(a.is_reference());
        r.set(Value::Int(5));
        assert_eq!(a.deref().as_int(), Some(5));
        let b = a.clone();
        assert_eq!(r.refcount(), 3);
        if let Value::Reference(rb) = &b {
            assert!(PhpRef::ptr_eq(rb, &r));
        }
        a.unwrap_reference();
        assert!(!a.is_reference());
        let guard = r.borrow();
        assert!(r.try_borrow_mut().is_none());
        drop(guard);
        assert!(r.try_borrow_mut().is_some());
        // nested references are flattened
        let nested = PhpRef::new(Value::Reference(PhpRef::new(Value::Int(2))));
        assert_eq!(nested.get().as_int(), Some(2));
    }

    #[test]
    fn objects_identity() {
        let o = Obj::new(TestObj(1));
        let p = o.clone();
        let q = Obj::new(TestObj(1));
        assert!(Obj::ptr_eq(&o, &p));
        assert!(!Obj::ptr_eq(&o, &q));
        assert!(o.downcast_ref::<TestObj>().is_some());
        assert_eq!(o.handle(), 1);
    }

    #[test]
    fn resources() {
        let r = Resource::with_handle(5, "stream", None);
        assert_eq!(r.handle(), 5);
        assert_eq!(r.type_name().unwrap(), "stream");
        r.close();
        assert!(r.is_closed());
        assert_eq!(Value::Resource(r).legacy_type_name(), "resource (closed)");
    }
}
