// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `SoapVar::__construct()` stores its name arguments with `ZVAL_STR_COPY`
//! (ext/soap/soap.c): the properties share the caller's strings, so a
//! 2 GiB node name (ext/soap/tests/soap_qname_crash.phpt) costs nothing
//! more under `memory_limit`.

mod common;

use common::TestHost;
use php_runtime::{Value, ZStr};
use php_soap::host::SoapHost;

#[test]
fn soapvar_names_share_the_argument_strings() {
    let mut h = TestHost::new();
    let o = h.new_object("SoapVar");
    let name = ZStr::from(&b"prefix:tag"[..]);
    let ns = ZStr::from(&b"urn:x"[..]);
    php_soap::objects::soap_var_construct(
        &mut h,
        &o,
        &Value::from("value"),
        Some(php_soap::consts::XSD_QNAME),
        None,
        None,
        Some(&name),
        Some(&ns),
    )
    .expect("a valid SoapVar");
    let Some(Value::Str(stored)) = h.read_slot(&o, "enc_name") else {
        panic!("enc_name must be a string");
    };
    assert!(ZStr::ptr_eq(&stored, &name));
    let Some(Value::Str(stored)) = h.read_slot(&o, "enc_namens") else {
        panic!("enc_namens must be a string");
    };
    assert!(ZStr::ptr_eq(&stored, &ns));
    // An empty name is not stored (`if (name && ZSTR_LEN(name) > 0)`).
    assert!(!matches!(h.read_slot(&o, "enc_stype"), Some(Value::Str(_))));
}
