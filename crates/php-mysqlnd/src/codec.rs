//! Row decoding into PHP values (text protocol `php_mysqlnd_rowp_read_text_protocol`
//! and binary protocol `php_mysqlnd_rowp_read_binary_protocol` +
//! `mysqlnd_ps_fetch_functions`) and the statistics they collect
//! (ref: `mysqlnd_wireprotocol.c`, `mysqlnd_ps_codec.c`).

use php_runtime::{Value, ZStr};

use crate::consts::{
    MYSQL_TYPE_BIT, MYSQL_TYPE_BLOB, MYSQL_TYPE_DATE, MYSQL_TYPE_DATETIME, MYSQL_TYPE_DECIMAL,
    MYSQL_TYPE_DOUBLE, MYSQL_TYPE_ENUM, MYSQL_TYPE_FLOAT, MYSQL_TYPE_GEOMETRY, MYSQL_TYPE_INT24,
    MYSQL_TYPE_JSON, MYSQL_TYPE_LONG, MYSQL_TYPE_LONGLONG, MYSQL_TYPE_LONG_BLOB,
    MYSQL_TYPE_MEDIUM_BLOB, MYSQL_TYPE_NEWDATE, MYSQL_TYPE_NEWDECIMAL, MYSQL_TYPE_NULL,
    MYSQL_TYPE_SET, MYSQL_TYPE_SHORT, MYSQL_TYPE_STRING, MYSQL_TYPE_TIME, MYSQL_TYPE_TIMESTAMP,
    MYSQL_TYPE_TINY, MYSQL_TYPE_TINY_BLOB, MYSQL_TYPE_VARCHAR, MYSQL_TYPE_VAR_STRING,
    MYSQL_TYPE_VECTOR, MYSQL_TYPE_YEAR, UNSIGNED_FLAG, ZEROFILL_FLAG,
};
use crate::diag;
use crate::protocol::Field;
use crate::stats::{inc_conn, Stat, Stats};
use crate::wire::{FieldLen, Reader};

/// PHP type mysqlnd associates with a column type (`perm_bind.php_type`)
/// plus its fixed binary width (`pack_len`, 0 = variable).
fn perm_bind(t: u8) -> (PhpType, usize) {
    match t {
        MYSQL_TYPE_NULL => (PhpType::Null, 0),
        MYSQL_TYPE_TINY => (PhpType::Long, 1),
        MYSQL_TYPE_SHORT | MYSQL_TYPE_YEAR => (PhpType::Long, 2),
        MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG => (PhpType::Long, 4),
        MYSQL_TYPE_LONGLONG | MYSQL_TYPE_BIT => (PhpType::Long, 8),
        MYSQL_TYPE_FLOAT => (PhpType::Double, 4),
        MYSQL_TYPE_DOUBLE => (PhpType::Double, 8),
        _ => (PhpType::String, 0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhpType {
    Null,
    Long,
    Double,
    String,
}

fn text_stat(t: u8) -> Stat {
    match t {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => Stat::TextTypeFetchedDecimal,
        MYSQL_TYPE_TINY => Stat::TextTypeFetchedInt8,
        MYSQL_TYPE_SHORT => Stat::TextTypeFetchedInt16,
        MYSQL_TYPE_LONG => Stat::TextTypeFetchedInt32,
        MYSQL_TYPE_FLOAT => Stat::TextTypeFetchedFloat,
        MYSQL_TYPE_DOUBLE => Stat::TextTypeFetchedDouble,
        MYSQL_TYPE_NULL => Stat::TextTypeFetchedNull,
        MYSQL_TYPE_TIMESTAMP => Stat::TextTypeFetchedTimestamp,
        MYSQL_TYPE_LONGLONG => Stat::TextTypeFetchedInt64,
        MYSQL_TYPE_INT24 => Stat::TextTypeFetchedInt24,
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => Stat::TextTypeFetchedDate,
        MYSQL_TYPE_TIME => Stat::TextTypeFetchedTime,
        MYSQL_TYPE_DATETIME => Stat::TextTypeFetchedDatetime,
        MYSQL_TYPE_YEAR => Stat::TextTypeFetchedYear,
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_STRING => {
            Stat::TextTypeFetchedString
        }
        MYSQL_TYPE_BIT => Stat::TextTypeFetchedBit,
        MYSQL_TYPE_ENUM => Stat::TextTypeFetchedEnum,
        MYSQL_TYPE_SET => Stat::TextTypeFetchedSet,
        MYSQL_TYPE_JSON => Stat::TextTypeFetchedJson,
        MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_VECTOR => Stat::TextTypeFetchedBlob,
        MYSQL_TYPE_GEOMETRY => Stat::TextTypeFetchedGeometry,
        _ => Stat::TextTypeFetchedOther,
    }
}

fn binary_stat(t: u8) -> Stat {
    match t {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => Stat::BinaryTypeFetchedDecimal,
        MYSQL_TYPE_TINY => Stat::BinaryTypeFetchedInt8,
        MYSQL_TYPE_SHORT => Stat::BinaryTypeFetchedInt16,
        MYSQL_TYPE_LONG => Stat::BinaryTypeFetchedInt32,
        MYSQL_TYPE_FLOAT => Stat::BinaryTypeFetchedFloat,
        MYSQL_TYPE_DOUBLE => Stat::BinaryTypeFetchedDouble,
        MYSQL_TYPE_NULL => Stat::BinaryTypeFetchedNull,
        MYSQL_TYPE_TIMESTAMP => Stat::BinaryTypeFetchedTimestamp,
        MYSQL_TYPE_LONGLONG => Stat::BinaryTypeFetchedInt64,
        MYSQL_TYPE_INT24 => Stat::BinaryTypeFetchedInt24,
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => Stat::BinaryTypeFetchedDate,
        MYSQL_TYPE_TIME => Stat::BinaryTypeFetchedTime,
        MYSQL_TYPE_DATETIME => Stat::BinaryTypeFetchedDatetime,
        MYSQL_TYPE_YEAR => Stat::BinaryTypeFetchedYear,
        MYSQL_TYPE_VARCHAR | MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_STRING => {
            Stat::BinaryTypeFetchedString
        }
        MYSQL_TYPE_BIT => Stat::BinaryTypeFetchedBit,
        MYSQL_TYPE_ENUM => Stat::BinaryTypeFetchedEnum,
        MYSQL_TYPE_SET => Stat::BinaryTypeFetchedSet,
        MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_VECTOR => Stat::BinaryTypeFetchedBlob,
        MYSQL_TYPE_GEOMETRY => Stat::BinaryTypeFetchedGeometry,
        _ => Stat::BinaryTypeFetchedOther,
    }
}

/// glibc `strtoll`: optional whitespace and sign, decimal digits, saturating.
#[must_use]
pub fn atoll(s: &[u8]) -> i64 {
    let (neg, digits) = split_sign(s);
    let mut v: i128 = 0;
    for &d in digits {
        v = (v * 10 + i128::from(d - b'0')).min(i128::from(i64::MAX) + 1);
    }
    let v = if neg { -v } else { v };
    #[allow(clippy::cast_possible_truncation)]
    let r = v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
    r
}

/// glibc `strtoull`: like [`atoll`] but unsigned (a leading '-' negates
/// modulo 2^64), saturating at `u64::MAX`.
#[must_use]
pub fn strtoull(s: &[u8]) -> u64 {
    let (neg, digits) = split_sign(s);
    let mut v: u64 = 0;
    for &d in digits {
        match v
            .checked_mul(10)
            .and_then(|x| x.checked_add(u64::from(d - b'0')))
        {
            Some(x) => v = x,
            None => return u64::MAX,
        }
    }
    if neg {
        v.wrapping_neg()
    } else {
        v
    }
}

fn split_sign(s: &[u8]) -> (bool, &[u8]) {
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'-' || s[i] == b'+') {
        neg = s[i] == b'-';
        i += 1;
    }
    let start = i;
    while i < s.len() && s[i].is_ascii_digit() {
        i += 1;
    }
    (neg, &s[start..i])
}

fn be_uint(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |acc, &x| (acc << 8) | u64::from(x))
}

fn le_uint(b: &[u8]) -> u64 {
    b.iter()
        .rev()
        .fold(0u64, |acc, &x| (acc << 8) | u64::from(x))
}

fn sign_extend(v: u64, bytes: usize) -> i64 {
    let shift = 64 - 8 * u32::try_from(bytes).unwrap_or(8);
    #[allow(clippy::cast_possible_wrap)]
    let r = ((v << shift) as i64) >> shift;
    r
}

/// `ps_fetch_from_1_to_8_bytes`: decodes an integer (little-endian; BIT
/// values big-endian) honouring UNSIGNED/ZEROFILL.
fn from_1_to_8_bytes(field: &Field, b: &[u8]) -> Value {
    let is_bit = field.type_ == MYSQL_TYPE_BIT;
    let n = b.len();
    if field.flags & UNSIGNED_FLAG != 0 {
        let uval: u64 = match n {
            8 | 4 | 3 | 2 if !is_bit => le_uint(b),
            1..=8 => {
                if n == 1 {
                    u64::from(b[0])
                } else {
                    be_uint(b)
                }
            }
            _ => 0,
        };
        if field.flags & ZEROFILL_FLAG != 0 {
            let w = field.length as usize;
            return Value::Str(ZStr::from(format!("{uval:0w$}").into_bytes()));
        }
        if n < 8 || i64::try_from(uval).is_ok() {
            #[allow(clippy::cast_possible_wrap)]
            return Value::Int(uval as i64);
        }
        return Value::Str(ZStr::from(uval.to_string().into_bytes()));
    }
    let lval: i64 = match n {
        8 | 4 | 3 | 2 | 1 => sign_extend(le_uint(b), n),
        _ => 0,
    };
    Value::Int(lval)
}

/// `mysql_float_to_double`
#[must_use]
pub fn float_to_double(f: f32, decimals: u8) -> f64 {
    let d = f64::from(f);
    let s = if decimals >= 31 {
        php_runtime::float::zend_gcvt(d, 6, b'.', b'e')
    } else if d.is_finite() {
        format!("{:.*}", usize::from(decimals), d)
    } else {
        return d;
    };
    php_runtime::numeric::strtod(s.as_bytes()).0
}

fn malformed(msg: &str) -> Option<Value> {
    diag::warn(msg.to_string());
    None
}

fn sint4(b: &[u8]) -> i32 {
    i32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Reads a variable-length value's length prefix (`php_mysqlnd_net_field_length`
/// + `ps_fetch_is_packet_over_read_with_variable_length`).
fn var_len<'a>(r: &mut Reader<'a>, pack_len: usize) -> Option<(&'a [u8], usize)> {
    let start = r.pos();
    let mut sub = Reader::new(&r.rest()[..pack_len.min(r.remaining())]);
    let len = match sub.field_len() {
        FieldLen::Len(l) => usize::try_from(l).unwrap_or(usize::MAX),
        FieldLen::Null | FieldLen::Invalid => usize::MAX,
    };
    let length_len = sub.pos();
    r.skip(length_len);
    let _ = start;
    if pack_len > 0 && len > pack_len - length_len {
        diag::warn("Malformed server packet. Field length pointing after the end of packet");
        return None;
    }
    let data = r.take(len)?;
    Some((data, len))
}

fn fmt_frac(decimals: u8, second_part: u64) -> Option<String> {
    if decimals > 0 && decimals < 7 {
        let div = 10u64.pow(6 - u32::from(decimals));
        #[allow(clippy::cast_possible_truncation)]
        let v = (second_part / div) as u32;
        Some(format!(".{:0w$}", v, w = usize::from(decimals)))
    } else {
        None
    }
}

/// Decodes one binary-protocol column value at the reader's position.
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
fn binary_value(field: &Field, r: &mut Reader<'_>) -> Option<Value> {
    let pack_len = r.remaining();
    let fixed = |r: &mut Reader<'_>, n: usize| -> Option<Value> {
        if n > pack_len {
            return malformed(
                "Malformed server packet. Field length pointing after the end of packet",
            );
        }
        let b = r.take(n)?;
        Some(from_1_to_8_bytes(field, b))
    };
    match field.type_ {
        MYSQL_TYPE_NULL => Some(Value::Null),
        MYSQL_TYPE_TINY => fixed(r, 1),
        MYSQL_TYPE_SHORT | MYSQL_TYPE_YEAR => fixed(r, 2),
        MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG => fixed(r, 4),
        MYSQL_TYPE_LONGLONG => fixed(r, 8),
        MYSQL_TYPE_FLOAT => {
            if pack_len < 4 {
                return malformed(
                    "Malformed server packet. Field length pointing after the end of packet",
                );
            }
            let b = r.take(4)?;
            let f = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            Some(Value::Float(float_to_double(f, field.decimals)))
        }
        MYSQL_TYPE_DOUBLE => {
            if pack_len < 8 {
                return malformed(
                    "Malformed server packet. Field length pointing after the end of packet",
                );
            }
            let b = r.take(8)?;
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            Some(Value::Float(f64::from_le_bytes(a)))
        }
        MYSQL_TYPE_TIME | MYSQL_TYPE_DATE | MYSQL_TYPE_DATETIME | MYSQL_TYPE_TIMESTAMP => {
            let (to, length) = var_len(r, pack_len)?;
            let s = match field.type_ {
                MYSQL_TYPE_TIME => {
                    let (mut neg, mut h, mut m, mut s, mut frac) = (false, 0u32, 0u32, 0u32, 0u64);
                    if length > 0 {
                        if length < 8 {
                            return malformed("Malformed server packet. Field length is too short for the field type");
                        }
                        neg = to[0] != 0;
                        #[allow(clippy::cast_sign_loss)]
                        let day = sint4(&to[1..5]) as u32;
                        h = u32::from(to[5]).wrapping_add(day.wrapping_mul(24));
                        m = u32::from(to[6]);
                        s = u32::from(to[7]);
                        #[allow(clippy::cast_sign_loss)]
                        if length >= 12 {
                            frac = u64::from(sint4(&to[8..12]) as u32);
                        }
                    }
                    let mut out = format!("{}{h:02}:{m:02}:{s:02}", if neg { "-" } else { "" });
                    if let Some(f) = fmt_frac(field.decimals, frac) {
                        out.push_str(&f);
                    }
                    out
                }
                MYSQL_TYPE_DATE => {
                    let (mut y, mut mo, mut d) = (0u32, 0u32, 0u32);
                    if length > 0 {
                        if length < 4 {
                            return malformed("Malformed server packet. Field length is too short for the field type");
                        }
                        #[allow(clippy::cast_sign_loss)]
                        {
                            y = i32::from(i16::from_le_bytes([to[0], to[1]])) as u32;
                        }
                        mo = u32::from(to[2]);
                        d = u32::from(to[3]);
                    }
                    format!("{y:04}-{mo:02}-{d:02}")
                }
                _ => {
                    let (mut y, mut mo, mut d, mut h, mut mi, mut s, mut frac) =
                        (0u32, 0u32, 0u32, 0u32, 0u32, 0u32, 0u64);
                    if length > 0 {
                        if length < 4 {
                            return malformed("Malformed server packet. Field length is too short for the field type");
                        }
                        #[allow(clippy::cast_sign_loss)]
                        {
                            y = i32::from(i16::from_le_bytes([to[0], to[1]])) as u32;
                        }
                        mo = u32::from(to[2]);
                        d = u32::from(to[3]);
                        if length >= 7 {
                            h = u32::from(to[4]);
                            mi = u32::from(to[5]);
                            s = u32::from(to[6]);
                        }
                        #[allow(clippy::cast_sign_loss)]
                        if length >= 11 {
                            frac = u64::from(sint4(&to[7..11]) as u32);
                        }
                    }
                    let mut out = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}");
                    if let Some(f) = fmt_frac(field.decimals, frac) {
                        out.push_str(&f);
                    }
                    out
                }
            };
            Some(Value::Str(ZStr::from(s.into_bytes())))
        }
        MYSQL_TYPE_BIT => {
            let (b, len) = var_len(r, pack_len)?;
            if len > pack_len {
                return malformed(
                    "Malformed server packet. Field length pointing after the end of packet",
                );
            }
            Some(from_1_to_8_bytes(field, b))
        }
        _ => {
            let (b, _) = var_len(r, pack_len)?;
            Some(Value::Str(ZStr::from(b)))
        }
    }
}

/// `php_mysqlnd_rowp_read_binary_protocol`. `None` on a malformed row
/// (a warning has been queued).
pub fn decode_binary_row(row: &[u8], fields: &[Field], stats: &mut Stats) -> Option<Vec<Value>> {
    let n = fields.len();
    let null_bytes = (n + 9) / 8;
    let mut r = Reader::new(row);
    r.skip(1);
    let nulls = r.take(null_bytes).unwrap_or_default().to_vec();
    let mut out = Vec::with_capacity(n);
    for (i, f) in fields.iter().enumerate() {
        let bit = i + 2;
        let is_null = nulls
            .get(bit / 8)
            .is_some_and(|b| b & (1 << (bit % 8)) != 0);
        let before = r.pos();
        let (v, stat) = if is_null {
            (Value::Null, Stat::BinaryTypeFetchedNull)
        } else {
            if r.remaining() == 0 {
                diag::warn("Malformed server packet. No packet space left for the field");
                return None;
            }
            (binary_value(f, &mut r)?, binary_stat(f.type_))
        };
        let bytes = match &v {
            Value::Str(s) => s.len() as u64,
            _ => (r.pos() - before) as u64,
        };
        inc_conn(stats, stat, 1);
        inc_conn(stats, Stat::BytesReceivedPureDataPs, bytes);
        out.push(v);
    }
    Some(out)
}

/// `php_mysqlnd_rowp_read_text_protocol`. `as_int_or_float` is
/// `MYSQLI_OPT_INT_AND_FLOAT_NATIVE`.
pub fn decode_text_row(
    row: &[u8],
    fields: &[Field],
    as_int_or_float: bool,
    stats: &mut Stats,
) -> Option<Vec<Value>> {
    let mut r = Reader::new(row);
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        let len = match r.field_len() {
            FieldLen::Null => {
                out.push(Value::Null);
                continue;
            }
            FieldLen::Len(l) => usize::try_from(l).unwrap_or(usize::MAX),
            FieldLen::Invalid => usize::MAX,
        };
        if len > r.remaining() {
            diag::warn("Malformed server packet. Field length pointing after end of packet");
            return None;
        }
        let data = r.take(len)?;
        inc_conn(stats, text_stat(f.type_), 1);
        inc_conn(stats, Stat::BytesReceivedPureDataText, len as u64);
        let (php_type, pack_len) = perm_bind(f.type_);
        let v = if f.type_ == MYSQL_TYPE_BIT {
            match from_1_to_8_bytes(f, data) {
                #[allow(clippy::cast_sign_loss)]
                Value::Int(i) if !as_int_or_float => {
                    Value::Str(ZStr::from((i as u64).to_string().into_bytes()))
                }
                v => v,
            }
        } else if as_int_or_float && php_type == PhpType::Long && f.flags & ZEROFILL_FLAG == 0 {
            if pack_len < 8 {
                Value::Int(atoll(data))
            } else {
                let v = strtoull(data);
                if f.flags & UNSIGNED_FLAG != 0 && v > i64::MAX as u64 {
                    Value::Str(ZStr::from(data))
                } else {
                    #[allow(clippy::cast_possible_wrap)]
                    Value::Int(v as i64)
                }
            }
        } else if as_int_or_float && php_type == PhpType::Double {
            Value::Float(php_runtime::numeric::strtod(data).0)
        } else {
            Value::Str(ZStr::from(data))
        };
        out.push(v);
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    fn field(t: u8, flags: u32) -> Field {
        Field {
            type_: t,
            flags,
            length: 11,
            ..Field::default()
        }
    }

    fn text_row(vals: &[Option<&[u8]>]) -> Vec<u8> {
        let mut b = Vec::new();
        for v in vals {
            match v {
                None => b.push(0xfb),
                Some(s) => crate::wire::put_lenenc_str(&mut b, s),
            }
        }
        b
    }

    #[test]
    fn text_strings_by_default() {
        let mut st = Stats::default();
        let f = [
            field(MYSQL_TYPE_LONG, 0),
            field(MYSQL_TYPE_DOUBLE, 0),
            field(MYSQL_TYPE_VAR_STRING, 0),
        ];
        let row = text_row(&[Some(b"42"), Some(b"1.5"), None]);
        let v = decode_text_row(&row, &f, false, &mut st).unwrap();
        assert_eq!(crate::render_all(&v), ["s:42", "s:1.5", "N"]);
    }

    #[test]
    fn text_native_types() {
        let mut st = Stats::default();
        let f = [
            field(MYSQL_TYPE_LONG, 0),
            field(MYSQL_TYPE_DOUBLE, 0),
            field(MYSQL_TYPE_LONGLONG, UNSIGNED_FLAG),
            field(MYSQL_TYPE_LONGLONG, 0),
            field(MYSQL_TYPE_LONG, ZEROFILL_FLAG),
            field(MYSQL_TYPE_NEWDECIMAL, 0),
        ];
        let row = text_row(&[
            Some(b"-42"),
            Some(b"1.5"),
            Some(b"18446744073709551615"),
            Some(b"-9223372036854775808"),
            Some(b"00007"),
            Some(b"1.10"),
        ]);
        let v = decode_text_row(&row, &f, true, &mut st).unwrap();
        assert_eq!(
            crate::render_all(&v),
            [
                "i:-42",
                "d:1.5",
                "s:18446744073709551615",
                "i:-9223372036854775808",
                "s:00007",
                "s:1.10"
            ]
        );
    }

    #[test]
    fn text_bit_values() {
        let mut st = Stats::default();
        let f = [field(MYSQL_TYPE_BIT, UNSIGNED_FLAG)];
        let row = text_row(&[Some(&[0x01, 0x02])]);
        assert_eq!(
            crate::render_all(&decode_text_row(&row, &f, false, &mut st).unwrap()),
            ["s:258"]
        );
        assert_eq!(
            crate::render_all(&decode_text_row(&row, &f, true, &mut st).unwrap()),
            ["i:258"]
        );
    }

    #[test]
    fn binary_row_types() {
        let mut st = Stats::default();
        let mut tf = field(MYSQL_TYPE_TIME, 0);
        tf.decimals = 3;
        let f = [
            field(MYSQL_TYPE_TINY, 0),
            field(MYSQL_TYPE_LONGLONG, UNSIGNED_FLAG),
            field(MYSQL_TYPE_NULL, 0),
            field(MYSQL_TYPE_DATETIME, 0),
            tf,
            field(MYSQL_TYPE_VAR_STRING, 0),
            field(MYSQL_TYPE_DATE, 0),
        ];
        let mut row = vec![0u8];
        // null bitmap: column 2 is NULL -> bit 4
        row.extend_from_slice(&[0b0001_0000, 0]);
        row.push(0xff); // tinyint -1
        row.extend_from_slice(&u64::MAX.to_le_bytes());
        row.extend_from_slice(&[7, 0xe8, 0x07, 2, 29, 13, 14, 15]); // 2024-02-29 13:14:15
        row.extend_from_slice(&[12, 1, 1, 0, 0, 0, 2, 3, 4, 0x40, 0xe2, 0x01, 0]); // -26:03:04.123456
        row.extend_from_slice(b"\x03abc");
        row.push(0); // zero date
        let v = decode_binary_row(&row, &f, &mut st).unwrap();
        assert_eq!(
            crate::render_all(&v),
            [
                "i:-1",
                "s:18446744073709551615",
                "N",
                "s:2024-02-29 13:14:15",
                "s:-26:03:04.123",
                "s:abc",
                "s:0000-00-00"
            ]
        );
    }

    #[test]
    fn malformed_binary_row() {
        let mut st = Stats::default();
        let f = [field(MYSQL_TYPE_VAR_STRING, 0)];
        let _ = diag::take();
        assert!(decode_binary_row(&[0, 0, 0x05, b'a'], &f, &mut st).is_none());
        assert_eq!(
            diag::take()[0].message,
            "Malformed server packet. Field length pointing after the end of packet"
        );
    }

    #[test]
    fn float_conversion() {
        assert_eq!(float_to_double(1.1f32, 31), 1.1);
        assert_eq!(float_to_double(1.1f32, 2), 1.1);
        assert_eq!(float_to_double(1.234_567_8_f32, 31), 1.23457);
        assert_eq!(float_to_double(0.333_333_34_f32, 4), 0.3333);
    }

    #[test]
    fn c_integer_parsing() {
        assert_eq!(atoll(b" -12x"), -12);
        assert_eq!(atoll(b"99999999999999999999"), i64::MAX);
        assert_eq!(strtoull(b"-1"), u64::MAX);
        assert_eq!(strtoull(b"184467440737095516150"), u64::MAX);
    }

    #[test]
    fn zerofill_binary() {
        let mut f = field(MYSQL_TYPE_LONG, UNSIGNED_FLAG | ZEROFILL_FLAG);
        f.length = 5;
        assert_eq!(
            crate::render(&from_1_to_8_bytes(&f, &7u32.to_le_bytes())),
            "s:00007"
        );
    }
}
