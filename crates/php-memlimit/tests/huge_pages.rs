// buzzs-rust-php-project - Copyright (c) 2026 Aleks <al-git001@none.at>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! `set_huge_pages(false)` takes `MADV_HUGEPAGE` back from the mapping that
//! holds the heap (mimalloc marks its arenas by default), as php-src uses
//! no huge pages without `USE_ZEND_ALLOC_HUGE_PAGES`.
#![cfg(target_os = "linux")]

php_memlimit::install_global_allocator!();

/// The `VmFlags` of the mapping in `/proc/self/smaps` that contains `addr`.
fn vm_flags(addr: usize) -> String {
    let smaps = std::fs::read_to_string("/proc/self/smaps").unwrap();
    let mut inside = false;
    for line in smaps.lines() {
        if let Some((range, _)) = line.split_once(' ') {
            if let Some((a, b)) = range.split_once('-') {
                if let (Ok(a), Ok(b)) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16))
                {
                    inside = a <= addr && addr < b;
                    continue;
                }
            }
        }
        if inside {
            if let Some(flags) = line.strip_prefix("VmFlags:") {
                return flags.trim().to_string();
            }
        }
    }
    panic!("no mapping holds {addr:#x}");
}

#[test]
fn the_heap_mapping_loses_its_huge_page_advice() {
    let block = Box::new([0u8; 64]);
    let addr = block.as_ptr() as usize;
    let before = vm_flags(addr);
    if !before.split(' ').any(|f| f == "hg") {
        eprintln!("SKIPPED: the heap mapping is not marked MADV_HUGEPAGE here ({before})");
        return;
    }
    php_memlimit::set_huge_pages(false);
    let after = vm_flags(addr);
    assert!(!after.split(' ').any(|f| f == "hg"), "{after}");
    assert!(after.split(' ').any(|f| f == "nh"), "{after}");
}
