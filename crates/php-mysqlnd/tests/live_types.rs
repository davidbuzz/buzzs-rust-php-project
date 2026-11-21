// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Every column type through the text and binary protocols, with and
//! without `MYSQLI_OPT_INT_AND_FLOAT_NATIVE`, plus field metadata —
//! compared with `/usr/bin/php8.5`'s mysqli on the same server.
#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::doc_markdown,
    clippy::format_collect
)]

mod common;

use common::*;
use php_mysqlnd::consts::*;
use php_mysqlnd::{Connection, OptionValue};
use php_runtime::error::{ErrorLevel, RtResult, RuntimeContext};

const COLUMNS: &str = "id INT PRIMARY KEY AUTO_INCREMENT,
 c_tiny TINYINT, c_utiny TINYINT UNSIGNED, c_small SMALLINT, c_usmall SMALLINT(5) UNSIGNED ZEROFILL,
 c_med MEDIUMINT, c_int INT, c_uint INT UNSIGNED, c_big BIGINT, c_ubig BIGINT UNSIGNED,
 c_float FLOAT, c_float2 FLOAT(7,3), c_double DOUBLE, c_dec DECIMAL(20,5),
 c_date DATE, c_time TIME, c_time3 TIME(3), c_dt DATETIME, c_dt6 DATETIME(6), c_ts TIMESTAMP NULL DEFAULT NULL, c_year YEAR,
 c_char CHAR(5), c_varchar VARCHAR(50), c_binary BINARY(4), c_varbinary VARBINARY(10),
 c_text TEXT, c_blob BLOB, c_tinyblob TINYBLOB, c_longtext LONGTEXT,
 c_enum ENUM('a','b'), c_set SET('x','y','z'), c_bit1 BIT(1), c_bit BIT(12), c_bit64 BIT(64),
 c_json JSON, c_geom GEOMETRY NULL";

const ROWS: &[&str] = &[
    "(1, -128, 255, -32768, 42, -8388608, -2147483648, 4294967295, -9223372036854775808, 18446744073709551615,
      3.14159, 1234.567, 2.718281828459045, -123456789012345.12345,
      '2024-02-29', '-838:59:59', '12:34:56.789', '9999-12-31 23:59:59', '2001-02-03 04:05:06.123456', '2020-01-01 00:00:00', 2155,
      'ab', 'hello \\'world\\'', 'a\\0b\\0', 'x\\0y', 'text', 'blob\\0data', '', REPEAT('L', 300),
      'b', 'x,z', b'1', b'101010101010', b'1111111111111111111111111111111111111111111111111111111111111111',
      '{\"k\": [1, 2]}', ST_GeomFromText('POINT(1 2)'))",
    "(2, 127, 0, 32767, 0, 8388607, 2147483647, 0, 9223372036854775807, 9223372036854775808,
      -0.5, 0.001, 1e300, 0.00001,
      '0000-00-00', '00:00:00', '-00:00:00.5', '1000-01-01 00:00:00', '1970-01-01 00:00:01.000001', NULL, 1901,
      'é', '日本語', 'abcd', '', '', '', NULL, '',
      'a', '', b'0', b'0', b'0',
      'null', NULL)",
    "(3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
      NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
      NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL)",
];

fn setup(c: &Connection, t: &str) {
    c.query(format!("DROP TABLE IF EXISTS {t}").as_bytes())
        .unwrap();
    c.query(format!("CREATE TABLE {t} ({COLUMNS}) CHARSET utf8mb4").as_bytes())
        .unwrap();
    for r in ROWS {
        c.query(format!("INSERT INTO {t} VALUES {r}").as_bytes())
            .unwrap_or_else(|e| panic!("{e}"));
    }
}

fn field_line(f: &php_mysqlnd::Field) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}|def|0|{}|{}|{}|{}|{}",
        h(&f.name),
        h(&f.org_name),
        h(&f.table),
        h(&f.org_table),
        h(&f.def),
        h(&f.db),
        f.length,
        f.charsetnr,
        f.flags,
        f.type_,
        f.decimals
    )
}

const PHP_FIELDS: &str = r#"function fl($f){ return bin2hex($f->name)."|".bin2hex($f->orgname)."|".bin2hex($f->table)."|".bin2hex($f->orgtable)."|".bin2hex($f->def)."|".bin2hex($f->db)."|".$f->catalog."|".$f->max_length."|".$f->length."|".$f->charsetnr."|".$f->flags."|".$f->type."|".$f->decimals; }"#;

#[test]
fn text_protocol_all_types() {
    let Some(c) = connect() else { return };
    let t = table("types_text");
    setup(&c, &t);
    let mut out = Vec::new();
    for native in [false, true] {
        c.set_option(
            opt::MYSQLND_OPT_INT_AND_FLOAT_NATIVE,
            OptionValue::Int(i64::from(native)),
        );
        let mut r = query(&c, &format!("SELECT * FROM {t} ORDER BY id")).unwrap();
        while let Some(row) = r.fetch_array(FETCH_ASSOC).unwrap() {
            out.push(ra(&row));
        }
        out.push(format!("lengths:{:?}", r.fetch_lengths()));
        for f in r.fetch_fields() {
            out.push(field_line(f));
        }
    }
    c.query(format!("DROP TABLE {t}").as_bytes()).unwrap();
    let mine = out.join("\n");
    if !have_php() {
        return;
    }
    let code = format!(
        r#"{PHP_FIELDS} $m = conn(); $t = "{t}"; $m->query("DROP TABLE IF EXISTS $t");
        $m->query("CREATE TABLE $t ({cols}) CHARSET utf8mb4");
        foreach ([{rows}] as $row) $m->query("INSERT INTO $t VALUES $row");
        $out = [];
        foreach ([false, true] as $native) {{
            $m->options(MYSQLI_OPT_INT_AND_FLOAT_NATIVE, $native);
            $r = $m->query("SELECT * FROM $t ORDER BY id");
            while ($row = $r->fetch_assoc()) $out[] = r($row);
            $out[] = "lengths:None";
            foreach ($r->fetch_fields() as $f) $out[] = fl($f);
        }}
        $m->query("DROP TABLE $t");
        echo implode("\n", $out);"#,
        cols = COLUMNS.replace('"', "\\\""),
        rows = ROWS
            .iter()
            .map(|r| format!("{:?}", r.replace('$', "\\$")))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_lines_eq(&mine, &php(&code));
}

struct Ctx;
impl RuntimeContext for Ctx {
    fn diagnostic(&mut self, _level: ErrorLevel, _message: &str) -> RtResult<()> {
        Ok(())
    }
}

#[test]
fn binary_protocol_all_types() {
    let Some(c) = connect() else { return };
    let t = table("types_bin");
    setup(&c, &t);
    let mut out = Vec::new();
    // buffered via get_result
    let st = c.stmt_init();
    st.prepare(format!("SELECT * FROM {t} ORDER BY id").as_bytes())
        .unwrap();
    assert_eq!(st.param_count(), 0);
    assert_eq!(st.field_count(), 36);
    st.execute(&mut Ctx).unwrap();
    let mut r = st.get_result().unwrap().unwrap();
    while let Some(row) = r.fetch_array(FETCH_BOTH).unwrap() {
        out.push(ra(&row));
    }
    for f in r.fetch_fields() {
        out.push(field_line(f));
    }
    // unbuffered via fetch()
    st.execute(&mut Ctx).unwrap();
    while let Some(row) = st.fetch().unwrap() {
        out.push(rrow(&row));
    }
    // store_result + fetch, num_rows, data_seek
    st.execute(&mut Ctx).unwrap();
    st.store_result().unwrap();
    out.push(format!("num_rows:{}", st.num_rows()));
    assert!(st.data_seek(2));
    while let Some(row) = st.fetch().unwrap() {
        out.push(rrow(&row));
    }
    let meta = st.result_metadata().unwrap();
    out.push(format!("meta_fields:{}", meta.num_fields()));
    st.close();
    c.query(format!("DROP TABLE {t}").as_bytes()).unwrap();
    let mine = out.join("\n");
    if !have_php() {
        return;
    }
    let code = format!(
        r#"{PHP_FIELDS} $m = conn(); $t = "{t}"; $m->query("DROP TABLE IF EXISTS $t");
        $m->query("CREATE TABLE $t ({cols}) CHARSET utf8mb4");
        foreach ([{rows}] as $row) $m->query("INSERT INTO $t VALUES $row");
        $out = [];
        $s = $m->prepare("SELECT * FROM $t ORDER BY id"); $s->execute(); $r = $s->get_result();
        while ($row = $r->fetch_array(MYSQLI_BOTH)) $out[] = r($row);
        foreach ($r->fetch_fields() as $f) $out[] = fl($f);
        $vars = array_fill(0, 36, null); $refs = []; foreach ($vars as $k => &$v) $refs[] = &$v; unset($v);
        $s->execute(); $s->bind_result(...$refs);
        while ($s->fetch()) $out[] = r($vars);
        $s->execute(); $s->store_result(); $out[] = "num_rows:".$s->num_rows; $s->data_seek(2);
        while ($s->fetch()) $out[] = r($vars);
        $out[] = "meta_fields:".$s->result_metadata()->field_count;
        $s->close(); $m->query("DROP TABLE $t");
        echo implode("\n", $out);"#,
        cols = COLUMNS.replace('"', "\\\""),
        rows = ROWS
            .iter()
            .map(|r| format!("{:?}", r.replace('$', "\\$")))
            .collect::<Vec<_>>()
            .join(",")
    );
    assert_lines_eq(&mine, &php(&code));
}
