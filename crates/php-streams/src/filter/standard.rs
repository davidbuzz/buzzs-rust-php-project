// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Registration of the filters bundled with `ext/standard`
//! (`PHP_MINIT_FUNCTION(standard_filters)`).

use std::rc::Rc;

use super::consumed::ConsumedFilterFactory;
use super::convert::ConvertFilterFactory;
use super::dechunk::DechunkFilterFactory;
use super::string::{StringFilterFactory, StringOp};
use super::FilterRegistry;

/// Registers the standard filters in PHP's order: `string.rot13`,
/// `string.toupper`, `string.tolower`, `convert.*`, `consumed`, `dechunk`.
/// Patterns that are already registered are left untouched.
pub fn register_standard_filters(reg: &mut FilterRegistry) {
    reg.register(
        "string.rot13",
        Rc::new(StringFilterFactory(StringOp::Rot13)),
    );
    reg.register(
        "string.toupper",
        Rc::new(StringFilterFactory(StringOp::ToUpper)),
    );
    reg.register(
        "string.tolower",
        Rc::new(StringFilterFactory(StringOp::ToLower)),
    );
    reg.register("convert.*", Rc::new(ConvertFilterFactory));
    reg.register("consumed", Rc::new(ConsumedFilterFactory));
    reg.register("dechunk", Rc::new(DechunkFilterFactory));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order() {
        let mut reg = FilterRegistry::new();
        register_standard_filters(&mut reg);
        assert_eq!(
            reg.names(),
            [
                "string.rot13",
                "string.toupper",
                "string.tolower",
                "convert.*",
                "consumed",
                "dechunk"
            ]
        );
        assert!(reg.create("convert.base64-encode", None, false).is_some());
        assert!(reg.create("string.toupper", None, false).is_some());
    }
}
