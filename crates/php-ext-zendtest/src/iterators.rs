//! iterators.c: `ZendTest\Iterators\TraversableTest`, an
//! `IteratorAggregate` whose native iterator yields 0..3 and reports each
//! operation.

use php_runtime::{Obj, Value};
use php_vm::ext::{
    acc, class_flags, Args, ClassBuilder, ClassHooks, ModuleBuilder, NativeIterator, VmResult,
};
use php_vm::Vm;

/// `test_traversable_it`.
struct TraversableIt {
    current: i64,
}

impl NativeIterator for TraversableIt {
    fn rewind(&mut self, vm: &mut Vm) -> VmResult<()> {
        vm.output(b"TraversableTest::rewind\n");
        self.current = 0;
        Ok(())
    }

    fn valid(&mut self, vm: &mut Vm) -> VmResult<bool> {
        vm.output(b"TraversableTest::valid\n");
        Ok(self.current < 4)
    }

    fn current(&mut self, vm: &mut Vm) -> VmResult<Value> {
        vm.output(b"TraversableTest::current\n");
        Ok(Value::Int(self.current))
    }

    fn key(&mut self, vm: &mut Vm) -> VmResult<Value> {
        vm.output(b"TraversableTest::key\n");
        Ok(Value::Int(self.current))
    }

    fn next(&mut self, vm: &mut Vm) -> VmResult<()> {
        vm.output(b"TraversableTest::next\n");
        self.current += 1;
        Ok(())
    }
}

/// `test_traversable_get_iterator`.
fn get_iterator(vm: &mut Vm, _o: &Obj, by_ref: bool) -> VmResult<Box<dyn NativeIterator>> {
    if by_ref {
        return Err(vm.throw(
            "Error",
            "An iterator cannot be used with foreach by reference",
        ));
    }
    Ok(Box::new(TraversableIt { current: 0 }))
}

fn construct(_vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    Ok(Value::Null)
}

/// `getIterator()`: an `InternalIterator` over the native iterator.
fn method_get_iterator(vm: &mut Vm, _a: &mut Args) -> VmResult<Value> {
    let o = vm.new_internal_iterator(Box::new(TraversableIt { current: 0 }))?;
    Ok(Value::Object(o))
}

pub(crate) fn register(m: &mut ModuleBuilder<'_>) {
    let vm = m.vm();
    ClassBuilder::new("ZendTest\\Iterators\\TraversableTest")
        .flags(class_flags::FINAL)
        .implements(vm, &["IteratorAggregate"])
        .method("__construct()", acc::PUBLIC, construct)
        .method("getIterator(): Iterator", acc::PUBLIC, method_get_iterator)
        .hooks(ClassHooks {
            get_iterator: Some(get_iterator),
            ..Default::default()
        })
        .register(vm);
}
