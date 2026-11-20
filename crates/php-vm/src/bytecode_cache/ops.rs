// buzzs-rust-php-project - Copyright (c) 2026 git001 (Aleks)
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Cache encoding of the instruction set: [`crate::opcode::Op`] and the
//! operand, place and table types its variants carry.
//!
//! Every enum is written as a tag (`u16` for [`Op`], `u8` for the small
//! enums; tags follow declaration order) followed by its fields in
//! declaration order. Encode and decode of each enum or struct are
//! generated from one field list by `enum_codec!` / `struct_codec!`,
//! so the two sides cannot drift apart; the encode `match` and the struct
//! destructuring are exhaustive, so a new variant or field is a compile
//! error here until it is given an encoding.
//!
//! ref: `ext/opcache/zend_file_cache.c` (`zend_file_cache_serialize_op_array`)

use super::codec::{Codec, DResult, DecodeError, EResult, Reader, Writer};
use crate::opcode::{
    ArgSrc, BinKind, CastKind, ClassSpec, FetchMode, ForeachKind, FramelessCall, IncDecKind,
    IncludeKind, LiveTmp, MatchKey, MatchTable, Op, Operand, Place, PlaceBase, Seg, StaticVarDecl,
    TryEntry, UnKind,
};

/// The integer type an enum tag is written as.
trait Tag: Sized {
    /// Appends the tag.
    fn put(w: &mut Writer, tag: Self);
    /// Reads a tag back.
    fn get(r: &mut Reader<'_>) -> DResult<Self>;
}

impl Tag for u8 {
    fn put(w: &mut Writer, tag: Self) {
        w.u8(tag);
    }
    fn get(r: &mut Reader<'_>) -> DResult<Self> {
        r.u8()
    }
}

impl Tag for u16 {
    fn put(w: &mut Writer, tag: Self) {
        let [lo, hi] = tag.to_le_bytes();
        w.u8(lo);
        w.u8(hi);
    }
    fn get(r: &mut Reader<'_>) -> DResult<Self> {
        let lo = r.u8()?;
        let hi = r.u8()?;
        Ok(u16::from_le_bytes([lo, hi]))
    }
}

/// Decodes one field; the field name only drives the macro repetition.
macro_rules! field {
    ($_field:ident, $r:ident) => {
        Codec::decode($r)?
    };
}

/// Implements [`Codec`] for an enum from one list of its variants, each
/// as `Unit = tag`, `Tuple(a, b) = tag` (any binding names) or
/// `Struct { field, .. } = tag` (every field, by name). The tag is
/// written as `$tag`; the fields follow in the listed order. Duplicate
/// tags are rejected at compile time (`unreachable_patterns` is denied).
macro_rules! enum_codec {
    ($ty:ident : $tag:ty {
        $( $var:ident $( ( $($tf:ident),* ) )? $( { $($sf:ident),* } )? = $n:literal ),* $(,)?
    }) => {
        impl Codec for $ty {
            fn encode(&self, w: &mut Writer) -> EResult {
                match self {
                    $( $ty::$var $( ( $($tf),* ) )? $( { $($sf),* } )? => {
                        <$tag as Tag>::put(w, $n);
                        $( $( Codec::encode($tf, w)?; )* )?
                        $( $( Codec::encode($sf, w)?; )* )?
                    } )*
                }
                Ok(())
            }

            #[deny(unreachable_patterns)]
            fn decode(r: &mut Reader<'_>) -> DResult<Self> {
                Ok(match <$tag as Tag>::get(r)? {
                    $( $n => $ty::$var
                        $( ( $( field!($tf, r) ),* ) )?
                        $( { $( $sf: field!($sf, r) ),* } )?, )*
                    _ => return Err(DecodeError(concat!("invalid ", stringify!($ty), " tag"))),
                })
            }
        }
    };
}

/// Implements [`Codec`] for a struct with public fields, written in the
/// listed order. The encoder destructures every field, so a new field is
/// a compile error until it is listed.
macro_rules! struct_codec {
    ($ty:ident { $($f:ident),* $(,)? }) => {
        impl Codec for $ty {
            fn encode(&self, w: &mut Writer) -> EResult {
                let $ty { $($f),* } = self;
                $( Codec::encode($f, w)?; )*
                Ok(())
            }

            fn decode(r: &mut Reader<'_>) -> DResult<Self> {
                Ok($ty { $( $f: Codec::decode(r)? ),* })
            }
        }
    };
}

enum_codec!(Operand: u8 {
    Cv(a) = 0,
    Tmp(a) = 1,
    TmpKeep(a) = 2,
    Lit(a) = 3,
    Unused = 4,
});

enum_codec!(ClassSpec: u8 {
    Named(a) = 0,
    SelfClass = 1,
    Parent = 2,
    Static = 3,
    Dyn(a) = 4,
});

enum_codec!(PlaceBase: u8 {
    Cv(a) = 0,
    VarVar(a) = 1,
    This = 2,
    StaticProp { class, name } = 3,
    Value(a) = 4,
    Global(a) = 5,
    GlobalsArray = 6,
});

enum_codec!(Seg: u8 {
    Dim(a) = 0,
    Append = 1,
    Prop(a) = 2,
});

struct_codec!(Place { base, segs });

enum_codec!(BinKind: u8 {
    Arith(a) = 0,
    Equal = 1,
    NotEqual = 2,
    Identical = 3,
    NotIdentical = 4,
    Less = 5,
    LessEqual = 6,
    Spaceship = 7,
});

enum_codec!(UnKind: u8 {
    Neg = 0,
    Plus = 1,
    Not = 2,
    BitNot = 3,
    Bool = 4,
});

enum_codec!(CastKind: u8 {
    Int = 0,
    Float = 1,
    String = 2,
    Array = 3,
    Object = 4,
    Bool = 5,
    Null = 6,
});

enum_codec!(IncDecKind: u8 {
    PreInc = 0,
    PreDec = 1,
    PostInc = 2,
    PostDec = 3,
});

enum_codec!(FetchMode: u8 {
    Read = 0,
    Is = 1,
    List = 2,
});

enum_codec!(IncludeKind: u8 {
    Include = 0,
    IncludeOnce = 1,
    Require = 2,
    RequireOnce = 3,
    Eval = 4,
});

enum_codec!(ArgSrc: u8 {
    Val(a) = 0,
    CallResult(a) = 1,
    Place(a) = 2,
    TempPlace(a) = 3,
});

struct_codec!(FramelessCall {
    name,
    display,
    args,
    vals,
    argc,
    dst,
});

enum_codec!(ForeachKind: u8 {
    Value = 0,
    Ref = 1,
});

enum_codec!(MatchKey: u8 {
    Int(a) = 0,
    Str(a) = 1,
});

/// Only the entries (in order) and the default are written; the lookup
/// maps are rebuilt by inserting the entries again, which yields the same
/// table because the encoded entries never repeat a key.
impl Codec for MatchTable {
    fn encode(&self, w: &mut Writer) -> EResult {
        self.entries.encode(w)?;
        self.default.encode(w)
    }

    fn decode(r: &mut Reader<'_>) -> DResult<Self> {
        let entries = Vec::<(MatchKey, u32)>::decode(r)?;
        let mut table = MatchTable::new(u32::decode(r)?);
        for (key, target) in entries {
            if !table.insert(key, target) {
                return Err(DecodeError("duplicate match key"));
            }
        }
        Ok(table)
    }
}

struct_codec!(LiveTmp { start, end, tmp });

struct_codec!(TryEntry {
    try_start,
    try_end,
    catch_start,
    catch_end,
    finally,
    try_tmps,
});

struct_codec!(StaticVarDecl { name, init });

enum_codec!(Op: u16 {
        Nop = 0,
        Move { src, dst } = 1,
        Bin { op, a, b, dst } = 2,
        Unary { op, a, dst } = 3,
        Cast { kind, a, dst } = 4,
        Rope { parts, dst } = 5,
        InitArray { dst } = 6,
        ArrayAdd { arr, key, val } = 7,
        ArrayAddRef { arr, key, place } = 8,
        ArrayUnpack { arr, src } = 9,
        ConstArrayUnpack { arr, src } = 10,
        Jmp(a) = 11,
        JmpZ(a, b) = 12,
        JmpNz(a, b) = 13,
        JmpZEx { a, dst, target } = 14,
        JmpNzEx { a, dst, target } = 15,
        JmpSet { a, dst, target } = 16,
        Coalesce { a, dst, target } = 17,
        JmpNull { a, dst, target } = 18,
        AssignCv { cv, val, dst } = 19,
        Assign { place, val, dst } = 20,
        AssignOp { place, op, val, dst } = 21,
        IncDec { place, kind, dst } = 22,
        AssignRef { place, src, dst } = 23,
        MakeRef { place, dst } = 24,
        ListFetchRef { place, dst } = 25,
        Fetch { place, mode, dst } = 26,
        Isset { place, dst } = 27,
        Empty { place, dst } = 28,
        ArrayKeyExists { array, dst } = 29,
        Unset { place } = 30,
        Global { cv, name } = 31,
        BindStatic { cv, idx, skip } = 32,
        InitStatic { cv, idx, val } = 33,
        FetchThis { dst } = 34,
        InitFCall { name, fallback, display } = 35,
        InitDynCall { callee } = 36,
        InitMethodCall { obj, name } = 37,
        InitStaticCall { class, name } = 38,
        InitParentHook { prop, set } = 39,
        New { class, dst, skip } = 40,
        Send { src } = 41,
        SendUnpack { src } = 42,
        SendNamed { name, src } = 43,
        DoCall { dst } = 44,
        CallableConvert { dst, const_expr } = 45,
        SendPlaceholder { name } = 46,
        CallablePartial { dst, variadic, const_args, name } = 47,
        Recv { n, cv } = 48,
        RecvInit { n, cv, skip } = 49,
        RecvDefault { n, cv, val } = 50,
        RecvVariadic { n, cv } = 51,
        VerifyReturn { val } = 52,
        Return { val } = 53,
        ReturnRef { place } = 54,
        ReturnByRefValue { val } = 55,
        GeneratorCreate = 56,
        Yield { key, val, dst } = 57,
        YieldRef { key, place, dst } = 58,
        YieldByRefValue { key, val, call, dst } = 59,
        YieldFrom { src, dst } = 60,
        DeclareFunction(a) = 61,
        DeclareClass(a) = 62,
        DeclareAnonClass { idx, dst } = 63,
        MakeClosure { idx, dst } = 64,
        DeclareConst { name, val, attrs } = 65,
        FetchConst { name, fallback, dst } = 66,
        FetchClassConst { class, name, dst } = 67,
        FetchClassName { class, dst, const_expr } = 68,
        InstanceOf { a, class, dst } = 69,
        Clone { a, dst } = 70,
        FuncNumArgs { dst } = 71,
        FuncGetArgs { dst } = 72,
        Strlen { a, dst } = 73,
        Count { a, dst, sizeof } = 74,
        TypeCheck { a, mask, dst } = 75,
        GetType { a, dst } = 76,
        GetClass { a, dst } = 77,
        GetCalledClass { dst } = 78,
        Defined { name, dst } = 79,
        InArray { a, set, strict, dst } = 80,
        KeyExists { key, array, dst } = 81,
        Echo(a) = 82,
        Exit(a) = 83,
        Free(a) = 84,
        Throw(a) = 85,
        Catch { classes, cv, next } = 86,
        Rethrow = 87,
        FastCall { fin, target } = 88,
        FastRet { fin } = 89,
        DiscardFinally { fin } = 90,
        BeginSilence { dst } = 91,
        EndSilence { saved } = 92,
        Include { kind, a, dst } = 93,
        FeReset { subject, place, kind, iter, end } = 94,
        FeFetch { iter, end, val, key } = 95,
        FeFree { iter } = 96,
        FeFreeOnReturn { iter, loop_at } = 97,
        MatchError { subject } = 98,
        Match { subject, table } = 99,
        TypeAssertArray { val, msg } = 100,
        Ticks(a) = 101,
        Diag { level, msg } = 102,
        UserOpcode { opcode } = 103,
        AssertCheck { dst, skip } = 104,
        ConstPropCheck { obj, name, dst } = 105,
        ConstDimCheck { obj } = 106,
        CmpJmp { op, a, b, dst, target, jmp_if, mask } = 107,
        BinAssignCv { op, a, b, dst, cv } = 108,
        IncDecCv { cv, kind, dst, place } = 109,
        AssignOpCv { cv, op, val, dst, place } = 110,
        AssignOpDimCv { cv, dim, op, val, place } = 111,
        IncDecDimCv { cv, dim, kind, dst, place } = 112,
        FetchDimCv { cv, dim, place, dst, to, mode } = 113,
        FetchDimsCv { cv, place, dst, to, mode } = 114,
        IssetDimCv { cv, dim, place, dst, empty } = 115,
        AssignDimCv { cv, dim, val, place } = 116,
        FetchStaticProp { place, dst, to, mode } = 117,
        FetchGlobal { place, dst, to, mode } = 118,
        AssignStaticProp { place, val, dst } = 119,
        AssignOpStaticProp { place, op, val, dst } = 120,
        IssetStaticProp { place, dst, empty } = 121,
        ConcatCv { cv, val, dst, place } = 122,
        AssignDimsCv { cv, val, place } = 123,
        AssignOpDimsCv { cv, op, val, place } = 124,
        FetchConstTo { name, fallback, dst, to } = 125,
        FetchClassConstTo { class, name, dst, to } = 126,
        IncDecCvCmpJmp { cv, kind, place } = 127,
        BinAssignCvCmpJmp { op, a, b, dst, cv } = 128,
        AssignOpCvCmpJmp { cv, op, val, place } = 129,
        BinAssignCvJmp { op, a, b, dst, cv, target, jmp_if } = 130,
        AssignOpCvJmp { cv, op, val, place, target, jmp_if } = 131,
        Abs { src, dst, to } = 132,
        FramelessICall(a) = 133,
        FetchObjProp { place, dst, to, mode } = 134,
        FetchObjPropDim { place, dim, dst, to, mode } = 135,
        AssignObjProp { place, val, dst } = 136,
        AssignObjPropDim { place, dim, val, dst } = 137,
        AssignOpObjProp { place, op, val, dst } = 138,
        AssignOpObjPropDim { place, dim, op, val, dst } = 139,
        IncDecObjProp { place, kind, dst } = 140,
        IncDecObjPropDim { place, dim, kind, dst } = 141,
        IssetObjProp { place, dst, empty } = 142,
        IssetObjPropDim { place, dim, dst, empty } = 143,
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::optimizer::DEFAULT_OPTIMIZATION_LEVEL;
    use crate::compiler::{compile, CompileOptions, CompiledUnit};
    use crate::func::UserFunction;
    use crate::opcode::OpArray;
    use php_runtime::{Value, ZStr};
    use std::collections::BTreeSet;
    use std::rc::Rc;

    fn compile_src(src: &str, level: u32, special_builtins: u32) -> CompiledUnit {
        let program =
            php_parser::parse(src.as_bytes(), &php_parser::ParseOptions::default()).expect("parse");
        let opts = CompileOptions {
            filename: "test.php".into(),
            optimization_level: level,
            special_builtins,
            ..CompileOptions::default()
        };
        compile(&program, Rc::from(src.as_bytes()), opts).expect("compile")
    }

    /// Every op array reachable from `f`: its own, its declared functions
    /// and closures, and its classes' methods and property hooks.
    fn collect(f: &Rc<UserFunction>, seen: &mut Vec<Rc<OpArray>>) {
        if seen.iter().any(|c| Rc::ptr_eq(c, &f.code)) {
            return;
        }
        seen.push(Rc::clone(&f.code));
        collect_decls(&f.code.decls, seen);
    }

    fn collect_decls(decls: &crate::opcode::DeclTable, seen: &mut Vec<Rc<OpArray>>) {
        for g in decls.functions.borrow().iter() {
            collect(g, seen);
        }
        for c in decls.classes.borrow().iter() {
            for m in &c.methods {
                collect(&m.func, seen);
            }
            for p in &c.props {
                if let Some(h) = &p.hooks {
                    for g in h.get.iter().chain(h.set.iter()) {
                        collect(g, seen);
                    }
                }
            }
        }
    }

    /// `Debug` of an op, with a match table's lookup maps (whose `Debug`
    /// order is random) left out: they are checked by lookups instead.
    fn show(op: &Op) -> String {
        match op {
            Op::Match { subject, table } => {
                format!("Match {subject:?} {:?} {}", table.entries, table.default)
            }
            other => format!("{other:?}"),
        }
    }

    /// The variant name of an op's `Debug` form.
    fn variant(op: &Op) -> String {
        format!("{op:?}")
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect()
    }

    fn roundtrip_op_array(code: &OpArray, names: &mut BTreeSet<String>) {
        let mut w = Writer::new();
        code.ops.encode(&mut w).expect("encode ops");
        code.try_table.encode(&mut w).expect("encode try table");
        code.live_tmps.encode(&mut w).expect("encode live tmps");
        code.places.encode(&mut w).expect("encode places");
        code.static_vars.encode(&mut w).expect("encode static vars");
        let (table, body) = w.finish();
        let mut r = Reader::new(&table, &body).expect("string table");
        let ops = Vec::<Op>::decode(&mut r).expect("decode ops");
        let try_table = Vec::<TryEntry>::decode(&mut r).expect("decode try table");
        let live_tmps = Vec::<LiveTmp>::decode(&mut r).expect("decode live tmps");
        let places = Vec::<Place>::decode(&mut r).expect("decode places");
        let static_vars = Vec::<StaticVarDecl>::decode(&mut r).expect("decode static vars");
        r.expect_end().expect("everything consumed");

        assert_eq!(ops.len(), code.ops.len());
        for (before, after) in code.ops.iter().zip(&ops) {
            assert_eq!(show(before), show(after));
            names.insert(variant(before));
            if let (Op::Match { table: t1, .. }, Op::Match { table: t2, .. }) = (before, after) {
                for (key, _) in &t1.entries {
                    let v = match key {
                        MatchKey::Int(i) => Value::Int(*i),
                        MatchKey::Str(s) => Value::Str(ZStr::from(&s[..])),
                    };
                    assert_eq!(t1.target(&v), t2.target(&v));
                }
                assert_eq!(t2.target(&Value::Null), t1.default);
            }
        }
        assert_eq!(format!("{:?}", code.try_table), format!("{try_table:?}"));
        assert_eq!(format!("{:?}", code.live_tmps), format!("{live_tmps:?}"));
        assert_eq!(format!("{:?}", code.places), format!("{places:?}"));
        assert_eq!(
            format!("{:?}", code.static_vars),
            format!("{static_vars:?}")
        );
    }

    const SNIPPETS: &[&str] = &[
        r"<?php
        function m($x) {
            return match ($x) { 1, 2 => 'small', 3 => 'three', 'a' => 'letter', 'b', 'c' => 'bc', default => 'other' };
        }
        function m2($x) { return match (true) { $x > 1 => 1, default => 2 }; }
        function m3($x) { return match ($x) { 1 => 'one', 2 => 'two' }; }
        echo m(1), m('a'), m3(2);
        ",
        r#"<?php
        function t() {
            try {
                throw new RuntimeException("x");
            } catch (RuntimeException | LogicException $e) {
                echo $e->getMessage();
            } catch (Throwable) {
                echo "t";
            } finally {
                echo "f";
            }
            try { return 1; } finally { echo "g"; }
        }
        [$a, [$b, $c]] = [1, [2, 3]];
        "#,
        r#"<?php
        function f(array $arr) {
            $sum = 0;
            foreach ($arr as $v) { $sum += $v; }
            foreach ($arr as $k => &$v) { $v = $k . $v; }
            unset($v);
            foreach ($arr as $k => [$p, $q]) { echo $p, $q; }
            for ($i = 0; $i < 10; $i++) { $sum -= $i; }
            while ($sum > 0) { --$sum; $sum--; ++$sum; }
            return $sum;
        }
        $mul = 3;
        $c = function ($x) use ($mul, &$sum) { static $n = 0, $m = [1, 2], $o; $n++; return $x * $mul; };
        $d = fn($y) => $y + $mul;
        echo "value {$c(2)} and $mul and {$arr['k']} and $obj->p\n";
        "#,
        r#"<?php
        function g($a, $obj) {
            $a->b['c'][] = 1;
            $a->b['c']['d'] .= "x";
            A::$sp = 2;
            A::$sp['k'] ??= 3;
            $name = 'v';
            $$name = 4;
            ${'w' . $name} = 5;
            $GLOBALS['gl'] = 6;
            $x = $obj?->m()?->p;
            $y = $obj?->q;
            $z = $a['x']['y'] ?? null;
            if (isset($a['x']->y, $z)) { unset($a['x']); }
            return empty($a->b) ? $x : $y;
        }
        "#,
        r"<?php
        function h(string $s, array $arr, $v) {
            $l = strlen($s);
            $c = str_contains($s, 'a');
            $i = in_array($v, [1, 2, 3], true) && in_array($s, ['x', 'y']);
            $j = in_array($v, $arr);
            $im = implode(',', $arr);
            $k = array_key_exists('k', $arr);
            $n = count($arr) + abs($v);
            $t = is_int($v) || is_string($v);
            $g = gettype($v) . get_class($this ?? new stdClass);
            return [(int)$s, (float)$s, (string)$v, (array)$v, (object)$arr, (bool)$v, -$v, +$v, ~$l, !$c, $l <=> $n, $l ** 2, $l % 3, $l << 1, $l ^ 1, $s . $l];
        }
        ",
        r"<?php
        include 'a.php';
        include_once 'b.php';
        require 'c.php';
        require_once __DIR__ . '/d.php';
        $r = eval('return 1;');
        $x = 1;
        $x += 2; $x -= 1; $x *= 3; $x /= 2; $x %= 5; $x **= 2; $x .= 'a'; $x |= 1; $x &= 3; $x ^= 1; $x <<= 1; $x >>= 1; $x ??= 0;
        $o = new ArrayObject([]);
        var_dump($o instanceof ArrayObject, $o instanceof $x, @file_get_contents('nope'));
        const FOO = 1;
        echo FOO, PHP_EOL, \M_PI;
        ",
        r"<?php
        function named(int $a, int $b = 2, int ...$rest) { return $a + $b + array_sum($rest); }
        echo named(b: 3, a: 1);
        echo named(...[1, 2, 3]);
        $args = ['a' => 1];
        echo named(...$args);
        $f = strlen(...);
        $g = named(?, 2);
        function &refret(array &$a) { return $a[0]; }
        function gen() {
            $x = yield 1;
            yield 'k' => $x;
            yield from [1, 2];
            return 3;
        }
        function &genref(array &$a) { foreach ($a as &$v) { yield $v; } }
        foreach (gen() as $k => $v) { echo $k, $v; }
        ",
        r"<?php
        interface Shape { public function area(): float; }
        abstract class Base implements Shape {
            public static int $count = 0;
            const NAME = 'base';
            public function __construct(protected readonly float $size) { static::$count++; self::$count += 0; }
            abstract public function area(): float;
            public static function make(float $s): static { return new static($s); }
            public function name(): string { return static::NAME . self::NAME . static::class; }
        }
        final class Square extends Base {
            const NAME = 'square';
            public string $label { get => 'sq' . $this->size; set => $value; }
            public function area(): float { return $this->size ** 2; }
            public function name(): string { return parent::name() . '!'; }
            public function __clone() { $this->label = 'c'; }
        }
        enum Suit: string {
            case Hearts = 'H';
            case Spades = 'S';
            public function color(): string { return match ($this) { self::Hearts => 'Red', self::Spades => 'Black' }; }
            public static function fromChar(string $c): self { return self::from($c); }
            const Wild = self::Spades;
        }
        $s = Square::make(2.0);
        $t = clone $s;
        echo $s->area(), $s->name(), Suit::Hearts->color(), Suit::from('S')->name, func_num_args();
        $anon = new class(1) extends Base { public function area(): float { return 0.0; } };
        list('a' => $la, 'b' => $lb) = ['a' => 1, 'b' => 2];
        switch ($la) { case 1: echo 'one'; break; case 'x': echo 'x'; break; default: echo 'd'; }
        exit(0);
        ",
    ];

    #[test]
    fn compiled_code_roundtrips() {
        let mut names = BTreeSet::new();
        for level in [0, DEFAULT_OPTIMIZATION_LEVEL] {
            for (src, special) in SNIPPETS.iter().flat_map(|s| [(s, 0), (s, u32::MAX)]) {
                let unit = compile_src(src, level, special);
                let mut arrays = Vec::new();
                collect(&unit.main, &mut arrays);
                collect_decls(&unit.decls, &mut arrays);
                for code in &arrays {
                    roundtrip_op_array(code, &mut names);
                }
            }
        }
        for expected in [
            "Match",
            "Catch",
            "FastCall",
            "FeReset",
            "FeFetch",
            "Rope",
            "Include",
            "Cast",
            "InstanceOf",
            "MakeClosure",
            "BindStatic",
            "Yield",
            "YieldFrom",
            "SendNamed",
            "SendUnpack",
            "AssignOp",
            "JmpNull",
            "FramelessICall",
            "Strlen",
            "InArray",
            "TypeCheck",
            "Count",
        ] {
            assert!(names.contains(expected), "no {expected} op in {names:?}");
        }
    }

    #[test]
    fn truncated_input_and_unknown_tags_are_errors() {
        let op = Op::Match {
            subject: Operand::Cv(0),
            table: Box::new({
                let mut t = MatchTable::new(7);
                t.insert(MatchKey::Int(1), 3);
                t.insert(MatchKey::Str(b"a".as_slice().into()), 4);
                t
            }),
        };
        let mut w = Writer::new();
        op.encode(&mut w).expect("encode");
        let (table, body) = w.finish();
        for cut in 0..body.len() {
            let mut r = Reader::new(&table, &body[..cut]).expect("string table");
            assert!(Op::decode(&mut r).is_err(), "decoded from {cut} bytes");
        }
        let mut r = Reader::new(&table, &body).expect("string table");
        assert!(Op::decode(&mut r).is_ok());

        let mut r = Reader::new(&table, &[0xff, 0xff]).expect("string table");
        assert_eq!(
            Op::decode(&mut r).err(),
            Some(DecodeError("invalid Op tag"))
        );
        let mut r = Reader::new(&table, &[200]).expect("string table");
        assert_eq!(
            Operand::decode(&mut r).err(),
            Some(DecodeError("invalid Operand tag"))
        );
        let mut r = Reader::new(&table, &[9]).expect("string table");
        assert!(CastKind::decode(&mut r).is_err());

        // A repeated match key cannot come from the encoder.
        let mut w = Writer::new();
        vec![(MatchKey::Int(1), 1_u32), (MatchKey::Int(1), 2)]
            .encode(&mut w)
            .expect("encode");
        0_u32.encode(&mut w).expect("encode");
        let (table, body) = w.finish();
        let mut r = Reader::new(&table, &body).expect("string table");
        assert!(MatchTable::decode(&mut r).is_err());
    }
}
