// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The `phpinfo()` section (`PHP_MINFO_FUNCTION(session)`).

use php_session::Serializer;
use php_vm::ext::InfoItem;
use php_vm::Vm;

use crate::host::with_host;

/// Names joined the way `session.c` builds them (each followed by a space).
fn joined<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let s = names.fold(String::new(), |mut acc, n| {
        acc.push_str(n);
        acc.push(' ');
        acc
    });
    if s.is_empty() {
        "none".to_string()
    } else {
        s
    }
}

/// `PHP_MINFO`: support row, save handlers, serializers, INI entries.
pub(crate) fn module_info(vm: &mut Vm) -> Vec<InfoItem> {
    let modules = with_host(vm, |h| h.state().module_names());
    let row = |a: &str, b: String| InfoItem::Row(vec![a.to_string(), b]);
    vec![
        InfoItem::TableStart,
        row("Session Support", "enabled".to_string()),
        row(
            "Registered save handlers",
            joined(modules.iter().map(String::as_str)),
        ),
        row(
            "Registered serializer handlers",
            joined(Serializer::ALL.iter().map(|s| s.name())),
        ),
        InfoItem::TableEnd,
        InfoItem::IniEntries,
    ]
}

#[cfg(test)]
mod tests {
    use super::joined;

    #[test]
    fn names_are_space_terminated() {
        assert_eq!(joined(["files", "user"].into_iter()), "files user ");
        assert_eq!(joined(std::iter::empty()), "none");
    }
}
