//! Build-script support shared by the C-library binding crates (php-curl,
//! php-gd, php-intl, php-xsl; each includes this file with `#[path]`):
//! binds a shared library lazily, on the first call of one of its
//! functions, instead of through `DT_NEEDED`.
//!
//! Why: every `php` start-up otherwise pays for loading and relocating the
//! library and its whole dependency tree (libcurl alone brings in gnutls,
//! libssh, libldap, kerberos, ...; libgd brings libheif, libtiff, ...),
//! about 11 M of the start-up's instructions, although few scripts call
//! these extensions. php-src avoids the same cost by building them as
//! separately loaded `.so` extensions.
//!
//! How (the "implib" technique): [`generate`] reads the library's exported
//! functions from its ELF `.dynsym`, keeps those whose names occur as
//! identifiers in the crate's sources (`src/`, `csrc/`), and writes
//! `$OUT_DIR/lazy_<tag>.rs`, which the crate `include!`s in its FFI module.
//! That file holds, for every kept function, a hidden-visibility x86-64
//! stub of the same name (`global_asm!`), so the crate's `extern "C"`
//! declarations and C glue link unchanged. A stub jumps through its slot;
//! an empty slot sends it to a shared trampoline that saves the argument
//! registers (including `%al` and `%xmm0-7`, so variadic and floating-point
//! functions work), calls the resolver (`dlopen` of the library's soname
//! once, then `dlsym`), stores the address in the slot and tail-jumps to
//! the real function with the caller's registers and stack intact. A
//! library that cannot be loaded is a fatal error naming it.
//!
//! When anything is unsupported (another target, the library not found or
//! not parseable, `PHPRUST_EAGER_LINK` set) [`generate`] writes a fallback
//! file with the same API (`lazy_<tag>_sym` resolving through
//! `RTLD_DEFAULT`) and returns `false`; the caller then links the library
//! the usual way.

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One library to bind lazily.
pub struct LazyLib<'a> {
    /// Identifier used in generated names (`lazy_<tag>_sym`, file name).
    pub tag: &'a str,
    /// Library base name: `curl` for `libcurl.so`.
    pub name: &'a str,
    /// pkg-config package whose `libdir` is searched first.
    pub pkg: Option<&'a str>,
    /// Version suffix the crate's declarations add through `link_name`
    /// (ICU's `_74`): a symbol also matches when its name minus this
    /// suffix occurs in the sources.
    pub suffix: Option<&'a str>,
}

/// Writes `$OUT_DIR/lazy_<tag>.rs`; `true` when stubs were generated (the
/// caller must then NOT link the library), `false` when the fallback was
/// written (the caller links the library normally).
pub fn generate(lib: &LazyLib<'_>) -> bool {
    println!("cargo:rerun-if-env-changed=PHPRUST_EAGER_LINK");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    for sub in ["src", "csrc"] {
        let d = manifest.join(sub);
        if d.is_dir() {
            println!("cargo:rerun-if-changed={}", d.display());
        }
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_default())
        .join(format!("lazy_{}.rs", lib.tag));
    let code = match lazy_code(lib, &manifest) {
        Ok(code) => code,
        Err(why) => {
            println!(
                "cargo:warning=lib{} is linked eagerly (no lazy binding: {why})",
                lib.name
            );
            fallback_code(lib.tag)
        }
    };
    let lazy = !code.starts_with("// eager");
    if let Err(e) = fs::write(&out, code) {
        panic!("cannot write {}: {e}", out.display());
    }
    lazy
}

fn lazy_code(lib: &LazyLib<'_>, manifest: &Path) -> Result<String, String> {
    if std::env::var_os("PHPRUST_EAGER_LINK").is_some() {
        return Err("PHPRUST_EAGER_LINK is set".into());
    }
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if arch != "x86_64" || os != "linux" {
        return Err(format!("target {arch}-{os} has no stub generator"));
    }
    let path = find_library(lib)?;
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let elf = parse_elf(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let soname = elf
        .soname
        .ok_or_else(|| format!("{} has no DT_SONAME", path.display()))?;
    let idents = source_idents(manifest);
    let wanted = |sym: &str| {
        idents.contains(sym)
            || lib
                .suffix
                .and_then(|s| sym.strip_suffix(s))
                .is_some_and(|base| idents.contains(base))
    };
    let names: Vec<&String> = elf.functions.iter().filter(|s| wanted(s)).collect();
    if names.is_empty() {
        return Err(format!(
            "no {} function is named in the sources",
            path.display()
        ));
    }
    Ok(stub_code(lib.tag, &soname, &names))
}

/// The library file `lib<name>.so` (the development link) in pkg-config's
/// libdir or the usual directories.
fn find_library(lib: &LazyLib<'_>) -> Result<PathBuf, String> {
    let file = format!("lib{}.so", lib.name);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(pkg) = lib.pkg {
        if let Ok(out) = Command::new("pkg-config")
            .args(["--variable=libdir", pkg])
            .output()
        {
            let d = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            if out.status.success() && !d.is_empty() {
                dirs.push(PathBuf::from(d));
            }
        }
    }
    dirs.extend(
        [
            "/usr/lib/x86_64-linux-gnu",
            "/lib/x86_64-linux-gnu",
            "/usr/lib64",
            "/usr/lib",
            "/usr/local/lib",
        ]
        .map(PathBuf::from),
    );
    dirs.iter()
        .map(|d| d.join(&file))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("{file} not found"))
}

/// Every identifier-like word in the crate's `.rs`, `.c` and `.h` sources.
fn source_idents(manifest: &Path) -> HashSet<String> {
    fn walk(dir: &Path, set: &mut HashSet<String>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, set);
            } else if p
                .extension()
                .is_some_and(|x| x == "rs" || x == "c" || x == "h")
            {
                if let Ok(text) = fs::read(&p) {
                    for w in text.split(|b| !(b.is_ascii_alphanumeric() || *b == b'_')) {
                        if !w.is_empty() {
                            set.insert(String::from_utf8_lossy(w).into_owned());
                        }
                    }
                }
            }
        }
    }
    let mut set = HashSet::new();
    for sub in ["src", "csrc"] {
        walk(&manifest.join(sub), &mut set);
    }
    set
}

struct Elf {
    soname: Option<String>,
    /// Defined, default- or protected-visibility global/weak functions.
    functions: BTreeSet<String>,
}

fn rd<const N: usize>(b: &[u8], off: usize) -> Result<[u8; N], String> {
    b.get(off..off + N)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| "truncated ELF file".to_owned())
}
fn u16_at(b: &[u8], off: usize) -> Result<usize, String> {
    Ok(usize::from(u16::from_le_bytes(rd(b, off)?)))
}
fn u32_at(b: &[u8], off: usize) -> Result<usize, String> {
    usize::try_from(u32::from_le_bytes(rd(b, off)?)).map_err(|e| e.to_string())
}
fn u64_at(b: &[u8], off: usize) -> Result<usize, String> {
    usize::try_from(u64::from_le_bytes(rd(b, off)?)).map_err(|e| e.to_string())
}
fn cstr_at(b: &[u8], off: usize) -> Result<String, String> {
    let s = b.get(off..).ok_or("string offset out of range")?;
    let end = s
        .iter()
        .position(|&c| c == 0)
        .ok_or("unterminated string")?;
    Ok(String::from_utf8_lossy(&s[..end]).into_owned())
}

/// Reads `.dynsym` and `DT_SONAME` of a little-endian ELF64 shared object.
fn parse_elf(b: &[u8]) -> Result<Elf, String> {
    if b.get(..4) != Some(b"\x7fELF") || b.get(4) != Some(&2) || b.get(5) != Some(&1) {
        return Err("not a little-endian ELF64 file (a linker script?)".into());
    }
    let shoff = u64_at(b, 0x28)?;
    let shentsize = u16_at(b, 0x3a)?;
    let shnum = u16_at(b, 0x3c)?;
    let sect = |i: usize| -> Result<(usize, usize, usize, usize, usize), String> {
        let h = shoff + i * shentsize;
        // (type, offset, size, link, entsize)
        Ok((
            u32_at(b, h + 4)?,
            u64_at(b, h + 24)?,
            u64_at(b, h + 32)?,
            u32_at(b, h + 40)?,
            u64_at(b, h + 56)?,
        ))
    };
    let mut elf = Elf {
        soname: None,
        functions: BTreeSet::new(),
    };
    let mut found_dynsym = false;
    for i in 0..shnum {
        let (ty, off, size, link, entsize) = sect(i)?;
        let strtab = sect(link)?.1;
        match ty {
            // SHT_DYNSYM
            11 => {
                found_dynsym = true;
                let entsize = if entsize == 0 { 24 } else { entsize };
                for s in (off..off + size).step_by(entsize).skip(1) {
                    let info = rd::<1>(b, s + 4)?[0];
                    let other = rd::<1>(b, s + 5)?[0];
                    let shndx = u16_at(b, s + 6)?;
                    let (ty, bind, vis) = (info & 0xf, info >> 4, other & 3);
                    // FUNC or GNU_IFUNC; GLOBAL or WEAK; DEFAULT or
                    // PROTECTED; defined.
                    if matches!(ty, 2 | 10)
                        && matches!(bind, 1 | 2)
                        && matches!(vis, 0 | 3)
                        && shndx != 0
                    {
                        let name = cstr_at(b, strtab + u32_at(b, s)?)?;
                        if !name.is_empty() && !name.starts_with('_') {
                            elf.functions.insert(name);
                        }
                    }
                }
            }
            // SHT_DYNAMIC
            6 => {
                for d in (off..off + size).step_by(16) {
                    let tag = u64_at(b, d)?;
                    if tag == 14 {
                        elf.soname = Some(cstr_at(b, strtab + u64_at(b, d + 8)?)?);
                    } else if tag == 0 {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    if !found_dynsym {
        return Err("no .dynsym section".into());
    }
    Ok(elf)
}

/// Shared Rust part of the generated file: `lazy_<tag>_sym` and the
/// fatal error. `LAZY_HANDLE` is the `dlopen` handle (lazy mode) or
/// `RTLD_DEFAULT` (eager mode).
fn common_code(tag: &str, soname: Option<&str>) -> String {
    let upper = tag.to_uppercase();
    let open = match soname {
        Some(so) => format!(
            r#"
static LAZY_{upper}_HANDLE: ::std::sync::OnceLock<usize> = ::std::sync::OnceLock::new();

/// The `dlopen` handle of {so}, opened on first use.
fn lazy_{tag}_handle() -> *mut ::core::ffi::c_void {{
    *LAZY_{upper}_HANDLE.get_or_init(|| {{
        // SAFETY: a NUL-terminated soname; RTLD_LAZY | RTLD_GLOBAL (0x101)
        // as the library would have been loaded through DT_NEEDED.
        let h = unsafe {{ lazy_{tag}_dl::dlopen(c"{so}".as_ptr(), 0x101) }};
        if h.is_null() {{
            lazy_{tag}_fatal("cannot load the shared library", c"{so}");
        }}
        h as usize
    }}) as *mut ::core::ffi::c_void
}}
"#
        ),
        None => format!(
            r"
/// `RTLD_DEFAULT`: the library is linked through `DT_NEEDED`.
fn lazy_{tag}_handle() -> *mut ::core::ffi::c_void {{
    ::core::ptr::null_mut()
}}
"
        ),
    };
    format!(
        r#"{open}
#[allow(clippy::upper_case_acronyms, dead_code)]
mod lazy_{tag}_dl {{
    extern "C" {{
        pub fn dlopen(file: *const ::core::ffi::c_char, mode: ::core::ffi::c_int) -> *mut ::core::ffi::c_void;
        pub fn dlsym(handle: *mut ::core::ffi::c_void, name: *const ::core::ffi::c_char) -> *mut ::core::ffi::c_void;
        pub fn dlerror() -> *const ::core::ffi::c_char;
    }}
}}

/// Fatal error for a library or symbol that cannot be bound at run time.
#[cold]
#[allow(dead_code)]
fn lazy_{tag}_fatal(what: &str, name: &::core::ffi::CStr) -> ! {{
    // SAFETY: dlerror returns NULL or a NUL-terminated message.
    let err = unsafe {{ lazy_{tag}_dl::dlerror() }};
    let detail = if err.is_null() {{
        ::std::borrow::Cow::Borrowed("")
    }} else {{
        // SAFETY: non-null and NUL-terminated (above).
        unsafe {{ ::core::ffi::CStr::from_ptr(err) }}.to_string_lossy()
    }};
    eprintln!("PHP Fatal error:  {{what}} {{}}: {{detail}}", name.to_string_lossy());
    ::std::process::exit(255)
}}

/// The address of `name` in the library (a data symbol, say), or null.
#[allow(dead_code)]
pub(crate) fn lazy_{tag}_sym(name: &::core::ffi::CStr) -> *mut ::core::ffi::c_void {{
    // SAFETY: a valid handle (or RTLD_DEFAULT) and a NUL-terminated name.
    unsafe {{ lazy_{tag}_dl::dlsym(lazy_{tag}_handle(), name.as_ptr()) }}
}}
"#
    )
}

fn fallback_code(tag: &str) -> String {
    format!(
        "// eager: generated by php-libxml/build/lazy_dl.rs (fallback)\n{}",
        common_code(tag, None)
    )
}

fn stub_code(tag: &str, soname: &str, names: &[&String]) -> String {
    let mut asm = String::new();
    let slots = format!(".Lphprs_lazy_{tag}_slots");
    let tramp = format!(".Lphprs_lazy_{tag}_tramp");
    asm.push_str(".pushsection .text.phprs_lazy_");
    asm.push_str(tag);
    asm.push_str(",\"ax\",@progbits\n");
    for (i, name) in names.iter().enumerate() {
        let _ = write!(
            asm,
            ".p2align 4\n.globl {name}\n.hidden {name}\n.type {name},@function\n{name}:\n\
             \tmovq {slots}+{off}(%rip), %r11\n\ttestq %r11, %r11\n\tjz .Lphprs_lazy_{tag}_s{i}\n\
             \tjmpq *%r11\n.Lphprs_lazy_{tag}_s{i}:\n\tmovl ${i}, %r11d\n\tjmp {tramp}\n\
             .size {name}, .-{name}\n",
            off = i * 8
        );
    }
    // Entered with the caller's arguments in place, the slot index in
    // %r11 and %rsp = 8 (mod 16) (the caller's return address on top).
    let _ = write!(
        asm,
        ".p2align 4\n{tramp}:\n\tsubq $200, %rsp\n\
         \tmovq %rdi, 0(%rsp)\n\tmovq %rsi, 8(%rsp)\n\tmovq %rdx, 16(%rsp)\n\tmovq %rcx, 24(%rsp)\n\
         \tmovq %r8, 32(%rsp)\n\tmovq %r9, 40(%rsp)\n\tmovq %rax, 48(%rsp)\n\tmovq %r11, 56(%rsp)\n\
         \tmovdqu %xmm0, 64(%rsp)\n\tmovdqu %xmm1, 80(%rsp)\n\tmovdqu %xmm2, 96(%rsp)\n\tmovdqu %xmm3, 112(%rsp)\n\
         \tmovdqu %xmm4, 128(%rsp)\n\tmovdqu %xmm5, 144(%rsp)\n\tmovdqu %xmm6, 160(%rsp)\n\tmovdqu %xmm7, 176(%rsp)\n\
         \tmovl %r11d, %edi\n\tcall {{resolve}}\n\
         \tmovq 56(%rsp), %r11\n\tleaq {slots}(%rip), %rcx\n\tmovq %rax, (%rcx,%r11,8)\n\tmovq %rax, %r11\n\
         \tmovq 0(%rsp), %rdi\n\tmovq 8(%rsp), %rsi\n\tmovq 16(%rsp), %rdx\n\tmovq 24(%rsp), %rcx\n\
         \tmovq 32(%rsp), %r8\n\tmovq 40(%rsp), %r9\n\tmovq 48(%rsp), %rax\n\
         \tmovdqu 64(%rsp), %xmm0\n\tmovdqu 80(%rsp), %xmm1\n\tmovdqu 96(%rsp), %xmm2\n\tmovdqu 112(%rsp), %xmm3\n\
         \tmovdqu 128(%rsp), %xmm4\n\tmovdqu 144(%rsp), %xmm5\n\tmovdqu 160(%rsp), %xmm6\n\tmovdqu 176(%rsp), %xmm7\n\
         \taddq $200, %rsp\n\tjmpq *%r11\n.popsection\n\
         .pushsection .bss.phprs_lazy_{tag},\"aw\",@nobits\n.p2align 3\n{slots}:\n\t.zero {size}\n.popsection\n",
        size = names.len() * 8
    );
    let mut list = String::new();
    for name in names {
        let _ = writeln!(list, "    c\"{name}\",");
    }
    format!(
        r######"// lazy: generated by php-libxml/build/lazy_dl.rs for {soname}
{common}
/// Names of the stubbed functions, by slot index.
static LAZY_{upper}_NAMES: [&::core::ffi::CStr; {n}] = [
{list}];

/// Called by the stub trampoline: the address of function `idx`.
extern "C" fn lazy_{tag}_resolve(idx: u32) -> usize {{
    let name = LAZY_{upper}_NAMES[idx as usize];
    let p = lazy_{tag}_sym(name);
    if p.is_null() {{
        lazy_{tag}_fatal("undefined symbol in {soname}:", name);
    }}
    p as usize
}}

// SAFETY: the stubs and the trampoline follow the SysV x86-64 ABI: they
// clobber only %r11 (and, on the first call, %rcx after saving it) before
// the tail jump, preserve every argument register including %al and
// %xmm0-7, and keep the stack aligned around the resolver call.
::core::arch::global_asm!(
    r#####"{asm}"#####,
    resolve = sym lazy_{tag}_resolve,
    options(att_syntax)
);
"######,
        common = common_code(tag, Some(soname)),
        upper = tag.to_uppercase(),
        n = names.len(),
    )
}
