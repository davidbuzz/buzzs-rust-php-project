//! `include`/`require` through stream wrappers
//! (`php_stream_open_for_zend_ex()`): installed as the VM's include opener.
//!
//! Plain paths are left to the VM unless `include_path` contains wrapper
//! entries. Openers installed later (e.g. phar) chain to this one.

use std::cell::Cell;

use php_runtime::ZStr;
use php_streams::diag::{self, Severity};
use php_streams::wrapper::{REPORT_ERRORS, STREAM_OPEN_FOR_INCLUDE, USE_PATH};
use php_vm::ext::{IncludeOpen, IncludeOpenerFn, VmResult};
use php_vm::Vm;

use crate::bridge;

thread_local! {
    static PREV: Cell<Option<IncludeOpenerFn>> = const { Cell::new(None) };
}

/// Installs the opener, chaining one installed before.
pub fn install(vm: &mut Vm) {
    PREV.with(|p| p.set(vm.include_opener));
    vm.set_include_opener(opener);
}

/// Whether `p` names a URL (`scheme://...` or `data:`).
fn is_url(p: &[u8]) -> bool {
    let n = php_streams::wrapper::scheme_len(p);
    n > 1
        && p.get(n) == Some(&b':')
        && (p[n + 1..].starts_with(b"//") || (n == 4 && p[..4].eq_ignore_ascii_case(b"data")))
}

fn opener(vm: &mut Vm, name: &[u8], kind: &str) -> VmResult<IncludeOpen> {
    let wrapped_path = vm
        .ini_string("include_path")
        .is_some_and(|ip| php_streams::util::find_bytes(&ip, b"://").is_some());
    if !is_url(name) && !wrapped_path {
        return match PREV.with(Cell::get) {
            Some(prev) => prev(vm, name, kind),
            None => Ok(IncludeOpen::NotHandled),
        };
    }
    let r = bridge::enter(vm, || {
        let mut opened = None;
        let s = php_streams::open::open_wrapper(
            name,
            "rb",
            USE_PATH | REPORT_ERRORS | STREAM_OPEN_FOR_INCLUDE,
            &mut opened,
            None,
        );
        s.map(|mut s| {
            let src = s.copy_to_mem(None).unwrap_or_default();
            s.close();
            (src, opened.or_else(|| s.orig_path.clone()))
        })
    });
    let pending = bridge::take_pending();
    for d in diag::take() {
        if pending.is_some() && d.code == diag::StreamErrorCode::OpenFailed {
            continue;
        }
        let level = match d.severity {
            Severity::Warning => 2,
            Severity::Notice => 8,
            Severity::Deprecated => 8192,
            Severity::Error => 1,
        };
        vm.raise(level, &format!("{kind}(): {}", d.text()))?;
    }
    if let Some(t) = pending {
        return Err(t);
    }
    Ok(match r {
        Some((source, opened)) => IncludeOpen::Opened {
            source,
            opened_path: ZStr::from(opened.unwrap_or_else(|| name.to_vec())),
        },
        None => IncludeOpen::Failed,
    })
}

#[cfg(test)]
mod tests {
    use super::is_url;

    #[test]
    fn urls() {
        assert!(is_url(b"test://x"));
        assert!(is_url(b"data:text/plain,x"));
        assert!(!is_url(b"/tmp/x.php"));
        assert!(!is_url(b"c:/x"));
    }
}
