// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! The INI directive registry (`EG(ini_directives)` and `Zend/zend_ini.c`):
//! registration with defaults, `ini_get()`, `ini_set()`, `ini_restore()`,
//! `ini_get_all()`, request deactivation, and the typed modify handlers of
//! PHP's core directives.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use super::quantity::{escape, parse_bool, parse_quantity, parse_uquantity, quantity_warning};
use crate::diag::{Diagnostic, DiagnosticSink, ErrorLevel};
use crate::numeric::{atoi, atol};

/// Access levels (`PHP_INI_USER`, `PHP_INI_PERDIR`, `PHP_INI_SYSTEM`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Access(pub u8);

impl Access {
    /// `PHP_INI_USER` — settable with `ini_set()`.
    pub const USER: Self = Self(1);
    /// `PHP_INI_PERDIR` — `.htaccess`/`.user.ini`.
    pub const PERDIR: Self = Self(2);
    /// `PHP_INI_SYSTEM` — `php.ini`/`-d`.
    pub const SYSTEM: Self = Self(4);
    /// `PHP_INI_ALL`.
    pub const ALL: Self = Self(7);

    /// Whether every bit of `other` is allowed … PHP checks `modifiable & type`.
    pub fn allows(self, modify_type: Self) -> bool {
        self.0 & modify_type.0 != 0
    }
}

/// Stages at which a value changes (`ZEND_INI_STAGE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Module startup (registration).
    Startup,
    /// Module shutdown.
    Shutdown,
    /// Request activation (per-dir / `-d` values).
    Activate,
    /// Request end (restoring modified values).
    Deactivate,
    /// `ini_set()` and friends.
    Runtime,
    /// `.htaccess`.
    Htaccess,
}

/// Typed value cached by a modify handler (what PHP stores in its globals).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum IniTyped {
    /// No typed value (plain string directive).
    #[default]
    None,
    /// Boolean directive.
    Bool(bool),
    /// Integer directive.
    Long(i64),
    /// Float directive.
    Double(f64),
}

/// The modify handler (`on_modify`) of a directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifyHandler {
    /// No handler: any value is accepted.
    None,
    /// `OnUpdateBool` (`zend_ini_parse_bool`).
    Bool,
    /// `OnUpdateLong` (`zend_ini_parse_quantity_warn`).
    Long,
    /// `OnUpdateLongGEZero`.
    LongGEZero,
    /// `OnUpdateReal` (`zend_strtod`).
    Real,
    /// `OnUpdateString` / `OnUpdateStr`.
    String,
    /// `OnUpdateStringUnempty` (rejects `""`).
    StringUnempty,
    /// `OnUpdateStrNotEmpty` (rejects `""`).
    StrNotEmpty,
    /// `OnSetPrecision` / `OnSetSerializePrecision` (`atol() >= -1`).
    Precision,
    /// `OnChangeMemoryLimit`.
    MemoryLimit,
    /// `OnChangeMaxMemoryLimit`.
    MaxMemoryLimit,
    /// `OnUpdateErrorReporting` (`atoi`, `E_ALL` when unset).
    ErrorReporting,
    /// `OnUpdateAssertions` (`zend.assertions`).
    Assertions,
    /// `OnUpdateDisplayErrors` (`on`/`stderr`/`stdout`/number).
    DisplayErrors,
    /// `OnUpdateTimeout` (`max_execution_time`, `atol`).
    Timeout,
    /// `OnSetExceptionStringParamMaxLen` (0..=1000000).
    ExceptionStringParamMaxLen,
    /// `OnSetLogFilter` (`all`/`no-ctrl`/`ascii`/`raw`).
    LogFilter,
    /// `OnUpdateFiberStackSize`.
    FiberStackSize,
    /// `OnUpdateGCEnabled` (`zend_ini_parse_bool`).
    GcEnabled,
    /// `OnUpdateMailCrLfMode` (`crlf`/`lf`/`mixed`/`os`).
    MailCrLfMode,
    /// `OnUpdateDefaultCharset` / `OnUpdateDefaultMimeTye`: a string that
    /// goes into the `Content-Type` header, so NUL, CR and LF are refused
    /// (bug #81518, header injection).
    HeaderValue,
    /// `OnUpdateAutoDetectLineEndings`: `OnUpdateBool` plus an
    /// "`auto_detect_line_endings` is deprecated" `E_DEPRECATED` for true values.
    AutoDetectLineEndings,
    /// `OnUpdateReportMemleaks` (main/main.c): `OnUpdateBool` plus a
    /// "Directive 'report_memleaks' is deprecated" docref `E_DEPRECATED`
    /// for false values.
    ReportMemleaks,
    /// `OnChangeMemoryHardLimitPercent` (main/main.c): a quantity that is
    /// -1 or at least 100 ("memory_hard_limit_percent must be >= 100, or
    /// -1" otherwise). Whether the allocator can take it is checked
    /// where the limit is applied.
    MemoryHardLimitPercent,
}

/// How `phpinfo()`, `php -i` and `--ri` show a directive's value (the
/// `ZEND_INI_DISP` callback of its `zend_ini_entry_def`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IniDisplayer {
    /// No displayer: `php_ini_displayer_cb()` prints the value (HTML
    /// escaped outside text mode), or "no value".
    Default,
    /// `zend_ini_boolean_displayer_cb` (every `*_INI_BOOLEAN` entry, and
    /// `zend_gc_enabled_displayer_cb`): "On" or "Off".
    Boolean,
    /// `php_ini_color_displayer_cb` (`highlight.*`).
    Color,
    /// main.c's `display_errors_mode` displayer: "On"/"Off"/"STDOUT"/
    /// "STDERR".
    DisplayErrors,
}

impl IniDisplayer {
    /// The displayer C gives a directive with this modify handler: every
    /// `OnUpdateBool` entry in php-src is declared with
    /// `STD_PHP_INI_BOOLEAN`, and the `highlight.*` colours with
    /// `php_ini_color_displayer_cb` (main/main.c).
    #[must_use]
    pub fn for_handler(name: &str, handler: ModifyHandler) -> IniDisplayer {
        match handler {
            ModifyHandler::Bool
            | ModifyHandler::GcEnabled
            | ModifyHandler::AutoDetectLineEndings
            | ModifyHandler::ReportMemleaks => IniDisplayer::Boolean,
            ModifyHandler::DisplayErrors => IniDisplayer::DisplayErrors,
            _ if name.starts_with("highlight.") => IniDisplayer::Color,
            _ => IniDisplayer::Default,
        }
    }

    /// The text this displayer prints for `value` (the local or the
    /// master value; `ZEND_INI_DISPLAY_ACTIVE`/`_ORIG`). `html` is false in
    /// text mode (the CLI's `phpinfo_as_text`). The colour displayer tests `zend_uv.html_errors`,
    /// which `php_module_startup()` sets to 1 and nothing changes, so it
    /// always prints the `<span>`.
    #[must_use]
    pub fn display(self, value: Option<&[u8]>, html: bool) -> Vec<u8> {
        // `ZSTR_VAL()`: the C displayers see the value up to its first NUL.
        let c_str = value.map(|v| v.split(|&c| c == 0).next().unwrap_or_default());
        match self {
            // `zend_ini_parse_bool()` and `php_get_display_errors_mode()`
            // compare the whole zend_string.
            IniDisplayer::Boolean => {
                if value.is_some_and(parse_bool) {
                    b"On".to_vec()
                } else {
                    b"Off".to_vec()
                }
            }
            IniDisplayer::DisplayErrors => display_errors_text(value).to_vec(),
            IniDisplayer::Color => match c_str {
                Some(v) => {
                    let mut s = b"<span style=\"color: ".to_vec();
                    s.extend_from_slice(v);
                    s.extend_from_slice(b"\">");
                    s.extend_from_slice(v);
                    s.extend_from_slice(b"</span>");
                    s
                }
                None => b"<i>no value</i>".to_vec(),
            },
            // php_ini_displayer_cb(): a value whose first byte is not NUL
            // is written whole (`PHPWRITE` with its length).
            IniDisplayer::Default => match value {
                Some(v) if v.first().is_some_and(|&c| c != 0) => {
                    if html {
                        html_puts(v)
                    } else {
                        v.to_vec()
                    }
                }
                _ if html => b"<i>no value</i>".to_vec(),
                _ => b"no value".to_vec(),
            },
        }
    }
}

/// `zend_html_puts()` as php-stdmisc's highlighter ports it.
fn html_puts(v: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len());
    for &c in v {
        match c {
            b'<' => out.extend_from_slice(b"&lt;"),
            b'>' => out.extend_from_slice(b"&gt;"),
            b'&' => out.extend_from_slice(b"&amp;"),
            b'\t' => out.extend_from_slice(b"    "),
            _ => out.push(c),
        }
    }
    out
}

/// main.c's `PHP_INI_DISP(display_errors_mode)`: the
/// `php_get_display_errors_mode()` mode of the value, as a word.
fn display_errors_text(value: Option<&[u8]>) -> &'static [u8] {
    match display_errors_mode(value) {
        0 => b"Off",
        2 => b"STDERR",
        _ => b"STDOUT",
    }
}

/// Static description of a directive (`zend_ini_entry_def`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniDirective {
    /// Directive name.
    pub name: String,
    /// Built-in default (`None` = `NULL`).
    pub default: Option<Vec<u8>>,
    /// Who may change it.
    pub access: Access,
    /// Modify handler.
    pub handler: ModifyHandler,
    /// Owning extension (`"Core"`, `"standard"`, …) for `ini_get_all()`.
    pub module: String,
    /// How phpinfo() shows the value.
    pub displayer: IniDisplayer,
    /// Whether C gives the entry an `on_modify` handler. Entries declared
    /// with a NULL one (`PHP_INI_ENTRY("SMTP", ..., NULL)`) cannot be
    /// restored at runtime: `zend_restore_ini_entry_cb()` starts from
    /// `FAILURE` and only an `on_modify` call changes that.
    pub has_on_modify: bool,
}

impl IniDirective {
    /// Convenience constructor.
    pub fn new(
        name: &str,
        default: Option<&str>,
        access: Access,
        handler: ModifyHandler,
        module: &str,
    ) -> Self {
        Self {
            name: name.to_string(),
            default: default.map(|d| d.as_bytes().to_vec()),
            access,
            handler,
            module: module.to_string(),
            displayer: IniDisplayer::for_handler(name, handler),
            has_on_modify: true,
        }
    }

    /// The same directive declared with a NULL `on_modify` in C (see
    /// [`IniDirective::has_on_modify`]).
    #[must_use]
    pub fn without_on_modify(mut self) -> Self {
        self.has_on_modify = false;
        self
    }

    /// The same directive with another displayer (an entry whose modify
    /// handler is its own but which C declares with `*_INI_BOOLEAN`, or
    /// with a `PHP_INI_ENTRY_EX` displayer).
    #[must_use]
    pub fn with_displayer(mut self, displayer: IniDisplayer) -> Self {
        self.displayer = displayer;
        self
    }
}

#[derive(Debug, Clone)]
struct Entry {
    def: IniDirective,
    value: Option<Vec<u8>>,
    orig_value: Option<Vec<u8>>,
    modifiable: Access,
    orig_modifiable: Access,
    modified: bool,
    typed: IniTyped,
}

/// One row of `ini_get_all()` with details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IniEntryInfo {
    /// Directive name.
    pub name: String,
    /// `global_value` (value before runtime modification).
    pub global_value: Option<Vec<u8>>,
    /// `local_value` (current value).
    pub local_value: Option<Vec<u8>>,
    /// `builtin_default_value`.
    pub builtin_default_value: Option<Vec<u8>>,
    /// `access` bitmask.
    pub access: u8,
    /// How phpinfo() shows the values.
    pub displayer: IniDisplayer,
}

impl IniEntryInfo {
    /// The local and the master value as `display_ini_entries()` shows
    /// them (`html` false in text mode).
    #[must_use]
    pub fn displayed(&self, html: bool) -> (Vec<u8>, Vec<u8>) {
        (
            self.displayer.display(self.local_value.as_deref(), html),
            self.displayer.display(self.global_value.as_deref(), html),
        )
    }
}

/// The `FxHash` multiply-rotate hash (rustc's): directive names are short
/// and looked up on every diagnostic, `assert()` and `ini_get()`, where the
/// default `SipHash` cost more than the rest of the lookup.
#[derive(Default, Clone, Copy)]
struct NameHasher(u64);

impl Hasher for NameHasher {
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            let mut w = [0u8; 8];
            w.copy_from_slice(c);
            self.0 = (self.0.rotate_left(5) ^ u64::from_le_bytes(w)).wrapping_mul(K);
        }
        for &b in chunks.remainder() {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(K);
        }
    }

    fn write_u8(&mut self, b: u8) {
        self.write(&[b]);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// A map keyed by directive name.
type NameMap<V> = HashMap<String, V, BuildHasherDefault<NameHasher>>;

/// The registry of INI directives.
#[derive(Debug, Clone)]
pub struct IniRegistry {
    entries: NameMap<Entry>,
    order: Vec<String>,
    /// Configuration values (`php.ini`, `-d`) consulted at registration.
    config: NameMap<Vec<u8>>,
    modified: Vec<String>,
    modules: Vec<String>,
    /// Current memory usage reported by `memory_limit` failures
    /// (`zend_memory_usage(true)`; PHP's heap starts with one 2 MiB chunk).
    pub memory_usage: u64,
    /// Changes whenever a directive's value may have changed (see
    /// [`IniRegistry::generation`]).
    generation: u64,
}

/// Source of process-unique registry generations.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
}

impl Default for IniRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// `zend_binary_strcasecmp()` ordering.
fn strcasecmp(a: &str, b: &str) -> std::cmp::Ordering {
    let la = a.bytes().map(|c| c.to_ascii_lowercase());
    let lb = b.bytes().map(|c| c.to_ascii_lowercase());
    la.cmp(lb)
}

impl IniRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self {
            entries: NameMap::default(),
            order: Vec::new(),
            config: NameMap::default(),
            modified: Vec::new(),
            modules: Vec::new(),
            memory_usage: 2 * 1024 * 1024,
            generation: next_generation(),
        }
    }

    /// A process-unique stamp of the current directive values: it changes
    /// on every operation that may change a value (registration, `alter`,
    /// restore, deactivation), and two distinct registries never share one
    /// unless one is an unmodified clone of the other. Callers that mirror
    /// INI values elsewhere re-read them only when it changes.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A registry with PHP's core directives registered, applying
    /// `config` (`php.ini` / `-d name=value`) values first.
    pub fn with_core<I, K, V>(config: I, diag: &mut dyn DiagnosticSink) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<Vec<u8>>,
    {
        let mut r = Self::new();
        for (k, v) in config {
            r.set_config(k, v);
        }
        r.register_all(core_directives(), diag);
        r
    }

    /// Adds a configuration-hash value (`php.ini`/`-d`).  It takes effect
    /// for directives registered afterwards.
    pub fn set_config(&mut self, name: impl Into<String>, value: impl Into<Vec<u8>>) {
        self.config.insert(name.into(), value.into());
    }

    /// A configuration-hash value (`get_cfg_var()`, and the first source
    /// for `${name}` in INI files).
    pub fn config_value(&self, name: &str) -> Option<&[u8]> {
        self.config.get(name).map(Vec::as_slice)
    }

    /// Declares that an extension exists (for `ini_get_all($ext)`).
    pub fn register_module(&mut self, module: &str) {
        if !self.modules.iter().any(|m| m.eq_ignore_ascii_case(module)) {
            self.modules.push(module.to_string());
        }
    }

    /// Registers several directives.
    pub fn register_all(
        &mut self,
        defs: impl IntoIterator<Item = IniDirective>,
        diag: &mut dyn DiagnosticSink,
    ) {
        for d in defs {
            self.register(d, diag);
        }
    }

    /// `zend_register_ini_entries()` for one directive.  Returns `false`
    /// if a directive of that name already exists.
    pub fn register(&mut self, def: IniDirective, diag: &mut dyn DiagnosticSink) -> bool {
        if self.entries.contains_key(&def.name) {
            return false;
        }
        self.generation = next_generation();
        self.register_module(&def.module);
        let name = def.name.clone();
        let mut entry = Entry {
            modifiable: def.access,
            def,
            value: None,
            orig_value: None,
            orig_modifiable: Access(0),
            modified: false,
            typed: IniTyped::None,
        };
        let configured = self.config.get(&name).cloned();
        let mut done = false;
        if let Some(cv) = configured {
            if let Ok((typed, over)) = self.on_modify(&entry, Some(&cv), Stage::Startup, diag) {
                entry.typed = typed;
                entry.value = Some(over.unwrap_or(cv));
                done = true;
            }
        }
        if !done {
            entry.value.clone_from(&entry.def.default);
            if let Ok((typed, over)) =
                self.on_modify(&entry, entry.value.clone().as_deref(), Stage::Startup, diag)
            {
                entry.typed = typed;
                if over.is_some() {
                    entry.value = over;
                }
            }
        }
        self.order.push(name.clone());
        self.entries.insert(name, entry);
        true
    }

    /// A startup `on_modify` handler rejected the configured value of `name`
    /// (`FAILURE` at `ZEND_INI_STAGE_STARTUP`): the directive keeps its
    /// built-in default. Returns `false` for an unknown directive.
    pub fn reset_to_default(&mut self, name: &str) -> bool {
        let Some(e) = self.entries.get_mut(name) else {
            return false;
        };
        self.generation = next_generation();
        e.value.clone_from(&e.def.default);
        e.typed = IniTyped::None;
        true
    }

    /// Whether a directive is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(name)
    }

    /// `zend_ini_get_value()` / `ini_get()`: `None` for unknown directives,
    /// `""` for registered directives without a value.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .get(name)
            .map(|e| e.value.as_deref().unwrap_or_default())
    }

    /// Whether a directive was modified since startup or the last
    /// [`IniRegistry::deactivate`] (it is in `EG(modified_ini_directives)`).
    pub fn is_modified(&self, name: &str) -> bool {
        self.entries.get(name).is_some_and(|e| e.modified)
    }

    /// The raw value (`None` when the directive has no value).
    pub fn raw(&self, name: &str) -> Option<&[u8]> {
        self.entries.get(name).and_then(|e| e.value.as_deref())
    }

    /// The value cached by the modify handler.
    pub fn typed(&self, name: &str) -> IniTyped {
        self.entries.get(name).map_or(IniTyped::None, |e| e.typed)
    }

    /// Integer view (`INI_INT()`-like: typed value or `atol()`).
    pub fn get_long(&self, name: &str) -> i64 {
        match self.typed(name) {
            IniTyped::Long(l) => l,
            IniTyped::Bool(b) => i64::from(b),
            #[allow(clippy::cast_possible_truncation)]
            IniTyped::Double(d) => d as i64,
            IniTyped::None => self.raw(name).map_or(0, atol),
        }
    }

    /// Boolean view (`INI_BOOL()`-like).
    pub fn get_bool(&self, name: &str) -> bool {
        match self.typed(name) {
            IniTyped::Bool(b) => b,
            IniTyped::Long(l) => l != 0,
            IniTyped::Double(d) => d != 0.0,
            IniTyped::None => self.raw(name).is_some_and(parse_bool),
        }
    }

    /// `zend_alter_ini_entry_ex()`.  Returns `false` on failure (unknown
    /// directive, insufficient access, or rejected by the handler).
    pub fn alter(
        &mut self,
        name: &str,
        new_value: &[u8],
        modify_type: Access,
        stage: Stage,
        force: bool,
        diag: &mut dyn DiagnosticSink,
    ) -> bool {
        let Some(entry) = self.entries.get(name) else {
            return false;
        };
        let modifiable = entry.modifiable;
        self.generation = next_generation();
        let modified = entry.modified;
        let mut entry = entry.clone();
        if stage == Stage::Activate && modify_type == Access::SYSTEM {
            entry.modifiable = Access::SYSTEM;
        }
        if !force && !entry.modifiable.allows(modify_type) {
            // The modifiable change above is kept, as in C.
            if let Some(e) = self.entries.get_mut(name) {
                e.modifiable = entry.modifiable;
            }
            return false;
        }
        if !modified {
            entry.orig_value.clone_from(&entry.value);
            entry.orig_modifiable = modifiable;
            entry.modified = true;
            self.modified.push(name.to_string());
        }
        let result = self.on_modify(&entry, Some(new_value), stage, diag);
        let ok = if let Ok((typed, over)) = result {
            entry.typed = typed;
            entry.value = Some(over.unwrap_or_else(|| new_value.to_vec()));
            true
        } else {
            false
        };
        self.entries.insert(name.to_string(), entry);
        ok
    }

    /// `ini_set($name, $value)`: returns the old value, or `None` (PHP's
    /// `false`) on failure.
    pub fn ini_set(
        &mut self,
        name: &str,
        value: &[u8],
        diag: &mut dyn DiagnosticSink,
    ) -> Option<Vec<u8>> {
        let old = self.get(name).map(<[u8]>::to_vec)?;
        if self.alter(name, value, Access::USER, Stage::Runtime, false, diag) {
            Some(old)
        } else {
            None
        }
    }

    /// `ini_get($name)`.
    pub fn ini_get(&self, name: &str) -> Option<Vec<u8>> {
        self.get(name).map(<[u8]>::to_vec)
    }

    fn restore_entry(&mut self, name: &str, stage: Stage, diag: &mut dyn DiagnosticSink) -> bool {
        let Some(entry) = self.entries.get(name) else {
            return false;
        };
        if !entry.modified {
            return true;
        }
        let entry = entry.clone();
        self.generation = next_generation();
        let result = self.on_modify(&entry, entry.orig_value.as_deref(), stage, diag);
        // "runtime failure is OK": the entry stays modified; without an
        // on_modify the result stays FAILURE.
        if stage == Stage::Runtime && (result.is_err() || !entry.def.has_on_modify) {
            return false;
        }
        let mut e = entry;
        if let Ok((typed, _)) = result {
            e.typed = typed;
        }
        e.value = e.orig_value.take();
        e.modifiable = e.orig_modifiable;
        e.modified = false;
        e.orig_modifiable = Access(0);
        self.entries.insert(name.to_string(), e);
        self.modified.retain(|n| n != name);
        true
    }

    /// `zend_restore_ini_entry()` / `ini_restore()`.
    pub fn restore(&mut self, name: &str, stage: Stage, diag: &mut dyn DiagnosticSink) -> bool {
        match self.entries.get(name) {
            None => false,
            Some(e) if stage == Stage::Runtime && !e.modifiable.allows(Access::USER) => false,
            Some(_) => self.restore_entry(name, stage, diag),
        }
    }

    /// `ini_restore($name)`.
    pub fn ini_restore(&mut self, name: &str, diag: &mut dyn DiagnosticSink) {
        self.restore(name, Stage::Runtime, diag);
    }

    /// The directives modified since startup or the last
    /// [`IniRegistry::deactivate`], in the order they were first modified
    /// (`EG(modified_ini_directives)`).
    #[must_use]
    pub fn modified_names(&self) -> Vec<String> {
        self.modified.clone()
    }

    /// `zend_ini_deactivate()`: restores every modified directive at the
    /// end of the request.
    pub fn deactivate(&mut self, diag: &mut dyn DiagnosticSink) {
        self.generation = next_generation();
        for name in std::mem::take(&mut self.modified) {
            if let Some(e) = self.entries.get(&name) {
                let mut e = e.clone();
                if e.modified {
                    if let Ok((typed, _)) =
                        self.on_modify(&e, e.orig_value.as_deref(), Stage::Deactivate, diag)
                    {
                        e.typed = typed;
                    }
                    e.value = e.orig_value.take();
                    e.modifiable = e.orig_modifiable;
                    e.modified = false;
                    self.entries.insert(name, e);
                }
            }
        }
    }

    /// `set_include_path()`: returns the old value or `None` on failure.
    pub fn set_include_path(
        &mut self,
        path: &[u8],
        diag: &mut dyn DiagnosticSink,
    ) -> Option<Vec<u8>> {
        let old = self.raw("include_path").map(<[u8]>::to_vec);
        if self.alter(
            "include_path",
            path,
            Access::USER,
            Stage::Runtime,
            false,
            diag,
        ) {
            old
        } else {
            None
        }
    }

    /// `zend_ini_sort_entries()`: persistently sorts the shared directive
    /// table case-insensitively, including entries outside a queried module.
    pub fn sort_entries(&mut self) {
        self.order.sort_by(|a, b| strcasecmp(a, b));
    }

    /// `ini_get_all($extension, $details)` rows, sorted like PHP
    /// (case-insensitive).  `Err` carries the "Extension \"%s\" cannot be
    /// found" warning. The PHP binding calls [`Self::sort_entries`] first;
    /// this read-only query does not change the shared table order.
    /// The rows are exactly the module's (`display_ini_entries()`); the
    /// PHP function itself uses [`Self::ini_get_all_function`].
    pub fn ini_get_all(&self, extension: Option<&str>) -> Result<Vec<IniEntryInfo>, Diagnostic> {
        self.rows(extension, true)
    }

    /// The rows of the PHP function `ini_get_all($extension)`. The Core
    /// module has module number 0, and `PHP_FUNCTION(ini_get_all)` filters
    /// only on a non-zero number (ext/standard/basic_functions.c), so
    /// `ini_get_all('core')` lists every extension's directives.
    pub fn ini_get_all_function(
        &self,
        extension: Option<&str>,
    ) -> Result<Vec<IniEntryInfo>, Diagnostic> {
        match extension {
            Some(ext) if ext.eq_ignore_ascii_case("core") => {
                // The module lookup still runs (Core is always registered).
                self.rows(Some(ext), true)?;
                self.rows(None, true)
            }
            _ => self.rows(extension, true),
        }
    }

    /// The same rows as [`Self::ini_get_all`] but in current table order
    /// (registration order until [`Self::sort_entries`] is called),
    /// as `php --re`/`--ri` and `ReflectionExtension::getINIEntries()`
    /// list them (`zend_ini_directives` iteration order).
    pub fn ini_entries_registered(
        &self,
        extension: Option<&str>,
    ) -> Result<Vec<IniEntryInfo>, Diagnostic> {
        self.rows(extension, false)
    }

    fn rows(&self, extension: Option<&str>, sorted: bool) -> Result<Vec<IniEntryInfo>, Diagnostic> {
        if let Some(ext) = extension {
            if !self.modules.iter().any(|m| m.eq_ignore_ascii_case(ext)) {
                return Err(Diagnostic::docref(
                    ErrorLevel::WARNING,
                    format!("Extension \"{ext}\" cannot be found"),
                ));
            }
        }
        let mut names: Vec<&String> = self
            .order
            .iter()
            .filter(|n| {
                extension.is_none_or(|ext| self.entries[*n].def.module.eq_ignore_ascii_case(ext))
            })
            .collect();
        if sorted {
            names.sort_by(|a, b| strcasecmp(a, b));
        }
        Ok(names
            .into_iter()
            .map(|n| {
                let e = &self.entries[n];
                IniEntryInfo {
                    name: n.clone(),
                    // PHP_FUNCTION(ini_get_all): orig_value, else value
                    // (a directive modified from NULL has no orig_value).
                    global_value: if e.modified {
                        e.orig_value.clone().or_else(|| e.value.clone())
                    } else {
                        e.value.clone()
                    },
                    local_value: e.value.clone(),
                    builtin_default_value: e.def.default.clone(),
                    access: e.modifiable.0,
                    displayer: e.def.displayer,
                }
            })
            .collect())
    }

    /// Runs the modify handler.  `Ok((typed, override))` on success where
    /// `override` replaces the stored string (only `memory_limit` does that).
    #[allow(clippy::too_many_lines)]
    fn on_modify(
        &self,
        entry: &Entry,
        new_value: Option<&[u8]>,
        stage: Stage,
        diag: &mut dyn DiagnosticSink,
    ) -> Result<(IniTyped, Option<Vec<u8>>), ()> {
        let name = entry.def.name.as_str();
        let warn = |diag: &mut dyn DiagnosticSink, msg: String| {
            diag.report(Diagnostic::new(ErrorLevel::WARNING, msg));
        };
        let quantity = |diag: &mut dyn DiagnosticSink, v: &[u8]| {
            let (val, err) = parse_quantity(v);
            if let Some(e) = err {
                warn(diag, quantity_warning(&escape(name.as_bytes()), &e));
            }
            val
        };
        let v = new_value.unwrap_or_default();
        Ok(match entry.def.handler {
            ModifyHandler::None | ModifyHandler::String => (IniTyped::None, None),
            ModifyHandler::Bool | ModifyHandler::GcEnabled => (IniTyped::Bool(parse_bool(v)), None),
            ModifyHandler::AutoDetectLineEndings => {
                let on = parse_bool(v);
                if on && stage != Stage::Deactivate {
                    diag.report(Diagnostic::new(
                        ErrorLevel::DEPRECATED,
                        "auto_detect_line_endings is deprecated",
                    ));
                }
                (IniTyped::Bool(on), None)
            }
            ModifyHandler::ReportMemleaks => {
                let on = parse_bool(v);
                // The CLI reports the startup value itself (php-cli's
                // startup.rs); the request-end restore is not reported.
                if !on && stage == Stage::Runtime {
                    diag.report(Diagnostic::docref(
                        ErrorLevel::DEPRECATED,
                        "Directive 'report_memleaks' is deprecated",
                    ));
                }
                (IniTyped::Bool(on), None)
            }
            ModifyHandler::MemoryHardLimitPercent => match new_value {
                Some(n) => {
                    let value = quantity(diag, n);
                    if value < 100 && value != -1 {
                        warn(
                            diag,
                            "memory_hard_limit_percent must be >= 100, or -1".into(),
                        );
                        return Err(());
                    }
                    (IniTyped::Long(value), None)
                }
                None => (IniTyped::Long(-1), None),
            },
            ModifyHandler::Long => (IniTyped::Long(quantity(diag, v)), None),
            ModifyHandler::LongGEZero => {
                let t = quantity(diag, v);
                if t < 0 {
                    return Err(());
                }
                (IniTyped::Long(t), None)
            }
            ModifyHandler::Real => (IniTyped::Double(strtod_prefix(v)), None),
            ModifyHandler::StringUnempty => {
                if new_value.is_some_and(|n| n.first().is_none_or(|&c| c == 0)) {
                    return Err(());
                }
                (IniTyped::None, None)
            }
            ModifyHandler::StrNotEmpty => {
                if new_value.is_some_and(<[u8]>::is_empty) {
                    return Err(());
                }
                (IniTyped::None, None)
            }
            ModifyHandler::Precision => {
                let i = atol(v);
                if i < -1 {
                    return Err(());
                }
                (IniTyped::Long(i), None)
            }
            ModifyHandler::ErrorReporting => (
                IniTyped::Long(
                    new_value.map_or(i64::from(ErrorLevel::ALL.0), |n| i64::from(atoi(n))),
                ),
                None,
            ),
            ModifyHandler::Timeout => (IniTyped::Long(atol(v)), None),
            ModifyHandler::ExceptionStringParamMaxLen => {
                let i = atol(v);
                if !(0..=1_000_000).contains(&i) {
                    return Err(());
                }
                (IniTyped::Long(i), None)
            }
            ModifyHandler::DisplayErrors => (IniTyped::Long(display_errors_mode(new_value)), None),
            ModifyHandler::LogFilter => {
                if !matches!(v, b"all" | b"no-ctrl" | b"ascii" | b"raw") {
                    return Err(());
                }
                (IniTyped::None, None)
            }
            ModifyHandler::HeaderValue => {
                if v.iter().any(|&c| matches!(c, 0 | b'\r' | b'\n')) {
                    return Err(());
                }
                (IniTyped::None, None)
            }
            ModifyHandler::MailCrLfMode => {
                // OnUpdateMailCrLfMode (main/main.c): an empty value is
                // accepted; a bad one is an E_WARNING at runtime and an
                // E_ERROR at the other stages, silent at deactivation.
                if !v.is_empty() && !matches!(v, b"crlf" | b"lf" | b"mixed" | b"os") {
                    if stage != Stage::Deactivate {
                        let level = if stage == Stage::Runtime {
                            ErrorLevel::WARNING
                        } else {
                            ErrorLevel::ERROR
                        };
                        diag.report(Diagnostic::docref(
                            level,
                            format!(
                                "Invalid value \"{}\" for mail.cr_lf_mode. Must be one of: \"crlf\", \"lf\", \"mixed\", \"os\"",
                                String::from_utf8_lossy(v)
                            ),
                        ));
                    }
                    return Err(());
                }
                (IniTyped::None, None)
            }
            ModifyHandler::FiberStackSize => match new_value {
                Some(n) => {
                    let t = quantity(diag, n);
                    if t < 0 {
                        warn(diag, "fiber.stack_size must be a positive number".into());
                        return Err(());
                    }
                    (IniTyped::Long(t), None)
                }
                None => (IniTyped::None, None),
            },
            ModifyHandler::Assertions => {
                let val = quantity(diag, v);
                let cur = match entry.typed {
                    IniTyped::Long(l) => l,
                    _ => val,
                };
                if !matches!(stage, Stage::Startup | Stage::Shutdown)
                    && cur != val
                    && (cur < 0 || val < 0)
                {
                    warn(
                        diag,
                        "zend.assertions may be completely enabled or disabled only in php.ini"
                            .into(),
                    );
                    return Err(());
                }
                (IniTyped::Long(val), None)
            }
            ModifyHandler::MaxMemoryLimit => {
                let value = match new_value {
                    Some(n) => Self::uquantity(name, n, diag),
                    None => 1 << 30,
                };
                if value < self.memory_usage {
                    diag.report(Diagnostic::new(
                        ErrorLevel::ERROR,
                        format!(
                            "Failed to set memory limit to {} bytes (Current memory usage is {} bytes)",
                            to_i64(value),
                            self.memory_usage
                        ),
                    ));
                    return Err(());
                }
                (IniTyped::Long(to_i64(value)), None)
            }
            ModifyHandler::MemoryLimit => {
                let value = match new_value {
                    Some(n) => Self::uquantity(name, n, diag),
                    None => 1 << 30,
                };
                let max = match self.typed("max_memory_limit") {
                    IniTyped::Long(m) => to_u64(m),
                    _ => u64::MAX,
                };
                if value > max {
                    if value != u64::MAX {
                        warn(
                            diag,
                            format!(
                                "Failed to set memory_limit to {} bytes. Setting to max_memory_limit instead (currently: {} bytes)",
                                to_i64(value),
                                to_i64(max)
                            ),
                        );
                    }
                    let over = self.raw("max_memory_limit").map(<[u8]>::to_vec);
                    return Ok((IniTyped::Long(to_i64(max)), over));
                }
                if value < self.memory_usage && stage != Stage::Deactivate {
                    warn(
                        diag,
                        format!(
                            "Failed to set memory limit to {} bytes (Current memory usage is {} bytes)",
                            to_i64(value),
                            self.memory_usage
                        ),
                    );
                    return Err(());
                }
                (IniTyped::Long(to_i64(value)), None)
            }
        })
    }

    fn uquantity(name: &str, v: &[u8], diag: &mut dyn DiagnosticSink) -> u64 {
        let (val, err) = parse_uquantity(v);
        if let Some(e) = err {
            diag.report(Diagnostic::new(
                ErrorLevel::WARNING,
                quantity_warning(&escape(name.as_bytes()), &e),
            ));
        }
        val
    }
}

fn to_i64(v: u64) -> i64 {
    i64::from_ne_bytes(v.to_ne_bytes())
}

fn to_u64(v: i64) -> u64 {
    u64::from_ne_bytes(v.to_ne_bytes())
}

/// `php_get_display_errors_mode()`: 0 off, 1 stdout, 2 stderr.
#[must_use]
pub fn display_errors_mode(value: Option<&[u8]>) -> i64 {
    let Some(v) = value else { return 1 };
    for (word, mode) in [
        (&b"on"[..], 1),
        (b"yes", 1),
        (b"true", 1),
        (b"stderr", 2),
        (b"stdout", 1),
    ] {
        if v.eq_ignore_ascii_case(word) {
            return mode;
        }
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mode = atol(v) as u8;
    if mode != 0 && mode != 1 && mode != 2 {
        1
    } else {
        i64::from(mode)
    }
}

/// `zend_strtod(s, NULL)`: the longest float prefix (after whitespace).
fn strtod_prefix(s: &[u8]) -> f64 {
    match crate::numeric::scan_numeric(s, true) {
        Some(r) => match r.value {
            #[allow(clippy::cast_precision_loss)]
            crate::numeric::NumericValue::Long(l) => l as f64,
            crate::numeric::NumericValue::Double(d) => d,
        },
        None => 0.0,
    }
}

/// PHP's core directives (`main/main.c`, `Zend/zend.c`, plus a few from
/// `ext/standard`) with their built-in defaults, access levels and
/// handlers.  The CLI SAPI additionally forces `display_errors=1`,
/// `html_errors=0`, `implicit_flush=1`, `output_buffering=0`,
/// `max_execution_time=0` and `max_input_time=-1` through the
/// configuration hash (see [`CLI_OVERRIDES`]).
#[allow(clippy::too_many_lines)]
pub fn core_directives() -> Vec<IniDirective> {
    use Access as A;
    use ModifyHandler as H;
    let pd_sys = Access(A::PERDIR.0 | A::SYSTEM.0);
    let c = |n: &str, d: Option<&str>, a: Access, h: ModifyHandler| {
        IniDirective::new(n, d, a, h, "Core")
    };
    let s = |n: &str, d: Option<&str>, a: Access, h: ModifyHandler| {
        IniDirective::new(n, d, a, h, "standard")
    };
    vec![
        c("highlight.comment", Some("#FF8000"), A::ALL, H::None).without_on_modify(),
        c("highlight.default", Some("#0000BB"), A::ALL, H::None).without_on_modify(),
        c("highlight.html", Some("#000000"), A::ALL, H::None).without_on_modify(),
        c("highlight.keyword", Some("#007700"), A::ALL, H::None).without_on_modify(),
        c("highlight.string", Some("#DD0000"), A::ALL, H::None).without_on_modify(),
        c("display_errors", Some("1"), A::ALL, H::DisplayErrors),
        c("display_startup_errors", Some("1"), A::ALL, H::Bool),
        c("enable_dl", Some("1"), A::SYSTEM, H::Bool),
        c("error_include_args", Some("0"), A::ALL, H::Bool),
        c("expose_php", Some("1"), A::SYSTEM, H::Bool),
        c("docref_root", Some(""), A::ALL, H::String),
        c("docref_ext", Some(""), A::ALL, H::String),
        c("html_errors", Some("1"), A::ALL, H::Bool),
        c("xmlrpc_errors", Some("0"), A::SYSTEM, H::Bool),
        c("xmlrpc_error_number", Some("0"), A::ALL, H::Long),
        c("max_input_time", Some("-1"), pd_sys, H::Long),
        c("ignore_user_abort", Some("0"), A::ALL, H::Bool),
        c("implicit_flush", Some("0"), A::ALL, H::Bool),
        c("log_errors", Some("0"), A::ALL, H::Bool),
        c("ignore_repeated_errors", Some("0"), A::ALL, H::Bool),
        c("ignore_repeated_source", Some("0"), A::ALL, H::Bool),
        c("report_memleaks", Some("1"), A::ALL, H::ReportMemleaks),
        c("report_zend_debug", Some("0"), A::ALL, H::Bool),
        c("output_buffering", Some("0"), pd_sys, H::Long),
        c("output_handler", None, pd_sys, H::StrNotEmpty),
        c("register_argc_argv", Some("0"), pd_sys, H::Bool),
        c("auto_globals_jit", Some("1"), pd_sys, H::Bool),
        c("short_open_tag", Some("1"), pd_sys, H::Bool),
        c("unserialize_callback_func", None, A::ALL, H::String),
        c("serialize_precision", Some("-1"), A::ALL, H::Precision),
        c("arg_separator.output", Some("&"), A::ALL, H::StrNotEmpty),
        c("arg_separator.input", Some("&"), pd_sys, H::StrNotEmpty),
        c("auto_append_file", None, pd_sys, H::String),
        c("auto_prepend_file", None, pd_sys, H::String),
        c("doc_root", None, A::SYSTEM, H::StrNotEmpty),
        c("default_charset", Some("UTF-8"), A::ALL, H::HeaderValue),
        c(
            "default_mimetype",
            Some("text/html"),
            A::ALL,
            H::HeaderValue,
        ),
        c("internal_encoding", None, A::ALL, H::String),
        c("input_encoding", None, A::ALL, H::String),
        c("output_encoding", None, A::ALL, H::String),
        c("error_log", None, A::ALL, H::String),
        c("error_log_mode", Some("0644"), A::ALL, H::Long),
        c(
            "extension_dir",
            Some("/usr/lib/php/20230831"),
            A::SYSTEM,
            H::StringUnempty,
        ),
        c("sys_temp_dir", None, A::SYSTEM, H::StrNotEmpty),
        c(
            "include_path",
            Some(".:/usr/share/php"),
            A::ALL,
            H::StringUnempty,
        ),
        c("max_execution_time", Some("30"), A::ALL, H::Timeout),
        c("open_basedir", None, A::ALL, H::String),
        c("file_uploads", Some("1"), A::SYSTEM, H::Bool),
        c("upload_max_filesize", Some("2M"), pd_sys, H::Long),
        c("post_max_size", Some("8M"), pd_sys, H::Long),
        c("upload_tmp_dir", None, A::SYSTEM, H::StringUnempty),
        c("max_input_nesting_level", Some("64"), pd_sys, H::LongGEZero),
        c("max_input_vars", Some("1000"), pd_sys, H::LongGEZero),
        c("user_dir", None, A::SYSTEM, H::String),
        c("variables_order", Some("EGPCS"), pd_sys, H::StringUnempty),
        c("request_order", None, pd_sys, H::String),
        c("error_append_string", None, A::ALL, H::String),
        c("error_prepend_string", None, A::ALL, H::String),
        c("SMTP", Some("localhost"), A::ALL, H::None).without_on_modify(),
        c("smtp_port", Some("25"), A::ALL, H::None).without_on_modify(),
        c("mail.add_x_header", Some("0"), pd_sys, H::Bool),
        c("mail.mixed_lf_and_crlf", Some("0"), pd_sys, H::Bool),
        c("mail.cr_lf_mode", Some("crlf"), pd_sys, H::MailCrLfMode),
        c("mail.log", None, pd_sys, H::String),
        c("browscap", None, A::SYSTEM, H::None),
        c("max_memory_limit", Some("-1"), A::SYSTEM, H::MaxMemoryLimit),
        c("memory_limit", Some("128M"), A::ALL, H::MemoryLimit),
        c(
            "memory_hard_limit_percent",
            None,
            A::ALL,
            H::MemoryHardLimitPercent,
        ),
        c("precision", Some("14"), A::ALL, H::Precision),
        c("sendmail_from", None, A::ALL, H::None).without_on_modify(),
        c(
            "sendmail_path",
            Some("/usr/sbin/sendmail -t -i"),
            A::SYSTEM,
            H::None,
        )
        .without_on_modify(),
        c("mail.force_extra_parameters", None, pd_sys, H::None),
        c("disable_functions", Some(""), A::SYSTEM, H::None).without_on_modify(),
        c("max_file_uploads", Some("20"), pd_sys, H::None).without_on_modify(),
        c("max_multipart_body_parts", Some("-1"), pd_sys, H::None).without_on_modify(),
        c("allow_url_fopen", Some("1"), A::SYSTEM, H::Bool),
        c("allow_url_include", Some("0"), A::SYSTEM, H::Bool),
        c("enable_post_data_reading", Some("1"), pd_sys, H::Bool),
        c("realpath_cache_size", Some("4096K"), A::SYSTEM, H::Long),
        c("realpath_cache_ttl", Some("120"), A::SYSTEM, H::Long),
        c("user_ini.filename", Some(".user.ini"), A::SYSTEM, H::String),
        c("user_ini.cache_ttl", Some("300"), A::SYSTEM, H::Long),
        c("hard_timeout", Some("2"), A::SYSTEM, H::Long),
        c("syslog.facility", Some("LOG_USER"), A::SYSTEM, H::None),
        c("syslog.ident", Some("php"), A::SYSTEM, H::String),
        c("syslog.filter", Some("no-ctrl"), A::ALL, H::LogFilter),
        // Zend/zend.c
        c("error_reporting", None, A::ALL, H::ErrorReporting),
        c("fatal_error_backtraces", Some("1"), A::ALL, H::Bool),
        c("zend.assertions", Some("1"), A::ALL, H::Assertions),
        c("zend.enable_gc", Some("1"), A::ALL, H::GcEnabled),
        c("zend.multibyte", Some("0"), A::PERDIR, H::Bool),
        c("zend.script_encoding", None, A::ALL, H::None),
        c("zend.detect_unicode", Some("1"), A::ALL, H::Bool),
        c("zend.signal_check", Some("0"), A::SYSTEM, H::Bool),
        c("zend.exception_ignore_args", Some("0"), A::ALL, H::Bool),
        c(
            "zend.exception_string_param_max_len",
            Some("15"),
            A::ALL,
            H::ExceptionStringParamMaxLen,
        ),
        c("fiber.stack_size", None, A::ALL, H::FiberStackSize),
        c("zend.max_allowed_stack_size", Some("0"), A::SYSTEM, H::Long),
        c("zend.reserved_stack_size", Some("0"), A::SYSTEM, H::Long),
        // ext/standard, in basic_functions.c's MINIT order: var.c,
        // file.c, assert.c, url_scanner_ex.re.
        s("unserialize_max_depth", Some("4096"), A::ALL, H::Long),
        s("user_agent", None, A::ALL, H::String),
        s("from", None, A::ALL, H::String),
        s("default_socket_timeout", Some("60"), A::ALL, H::Long),
        s(
            "auto_detect_line_endings",
            Some("0"),
            A::ALL,
            H::AutoDetectLineEndings,
        ),
        s("assert.active", Some("1"), A::ALL, H::Bool),
        s("assert.bail", Some("0"), A::ALL, H::Bool),
        s("assert.warning", Some("1"), A::ALL, H::Bool),
        s("assert.callback", None, A::ALL, H::None),
        s("assert.exception", Some("1"), A::ALL, H::Bool),
        s(
            "session.trans_sid_tags",
            Some("a=href,area=href,frame=src,form="),
            A::ALL,
            H::String,
        ),
        s("session.trans_sid_hosts", Some(""), A::ALL, H::String),
        s("url_rewriter.tags", Some("form="), A::ALL, H::String),
        s("url_rewriter.hosts", Some(""), A::ALL, H::String),
    ]
}

/// Configuration values the CLI SAPI applies (`HARDCODED_INI` and
/// `sapi_cli_ini_defaults()` in `sapi/cli/php_cli.c`). php-src master no
/// longer hardcodes `register_argc_argv=1`: the CLI registers `$argv` and
/// `$argc` from `SG(request_info).argc` whatever its value.
pub const CLI_OVERRIDES: &[(&str, &str)] = &[
    ("display_errors", "1"),
    ("html_errors", "0"),
    ("implicit_flush", "1"),
    ("output_buffering", "0"),
    ("max_execution_time", "0"),
    ("max_input_time", "-1"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn core() -> IniRegistry {
        let mut d: Vec<Diagnostic> = Vec::new();
        let r = IniRegistry::with_core(CLI_OVERRIDES.iter().map(|&(k, v)| (k, v)), &mut d);
        assert!(d.is_empty(), "{d:?}");
        r
    }

    #[test]
    fn defaults() {
        let r = core();
        assert_eq!(r.get("precision"), Some(&b"14"[..]));
        assert_eq!(r.get_long("precision"), 14);
        assert_eq!(r.get("serialize_precision"), Some(&b"-1"[..]));
        assert_eq!(r.get_long("serialize_precision"), -1);
        assert_eq!(r.get("memory_limit"), Some(&b"128M"[..]));
        assert_eq!(r.get_long("memory_limit"), 128 << 20);
        assert_eq!(r.get("display_errors"), Some(&b"1"[..]));
        assert_eq!(r.get("html_errors"), Some(&b"0"[..]));
        assert_eq!(r.get("implicit_flush"), Some(&b"1"[..]));
        assert!(r.get_bool("implicit_flush"));
        assert_eq!(r.get("max_execution_time"), Some(&b"0"[..]));
        assert_eq!(r.get("include_path"), Some(&b".:/usr/share/php"[..]));
        assert_eq!(r.get("zend.assertions"), Some(&b"1"[..]));
        assert_eq!(r.get("short_open_tag"), Some(&b"1"[..]));
        assert_eq!(r.get("error_reporting"), Some(&b""[..]));
        assert_eq!(r.raw("error_reporting"), None);
        assert_eq!(r.get_long("error_reporting"), 30719);
        assert_eq!(r.get("nonexistent"), None);
        assert_eq!(r.get("output_handler"), Some(&b""[..]));
    }

    #[test]
    fn generation_changes_with_values() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        let g0 = r.generation();
        assert_eq!(r.get("precision"), Some(&b"14"[..]));
        assert_eq!(r.generation(), g0, "reads keep the generation");
        assert_ne!(core().generation(), g0, "distinct registries differ");
        assert_eq!(r.clone().generation(), g0);
        r.ini_set("precision", b"6", &mut d);
        let g1 = r.generation();
        assert_ne!(g1, g0);
        r.ini_restore("precision", &mut d);
        let g2 = r.generation();
        assert_ne!(g2, g1);
        r.ini_set("include_path", b"/x", &mut d);
        assert_ne!(r.generation(), g2);
        let g3 = r.generation();
        r.deactivate(&mut d);
        assert_ne!(r.generation(), g3);
        assert_eq!(r.get("include_path"), Some(&b".:/usr/share/php"[..]));
    }

    #[test]
    fn is_modified_follows_alter_restore_and_deactivate() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert!(!r.is_modified("precision"));
        assert!(!r.is_modified("no.such.directive"));
        let g = r.generation();
        assert!(r.restore("precision", Stage::Deactivate, &mut d));
        assert_eq!(
            r.generation(),
            g,
            "restoring an unmodified entry changes nothing"
        );
        r.ini_set("precision", b"6", &mut d);
        assert!(r.is_modified("precision"));
        r.ini_restore("precision", &mut d);
        assert!(!r.is_modified("precision"));
        r.ini_set("precision", b"7", &mut d);
        r.deactivate(&mut d);
        assert!(!r.is_modified("precision"));
        assert_eq!(r.get("precision"), Some(&b"14"[..]));
    }

    #[test]
    fn ini_set_and_restore() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(r.ini_set("precision", b"6", &mut d), Some(b"14".to_vec()));
        assert_eq!(r.get("precision"), Some(&b"6"[..]));
        assert_eq!(r.get_long("precision"), 6);
        assert_eq!(r.ini_set("precision", b"-2", &mut d), None);
        assert_eq!(r.get("precision"), Some(&b"6"[..]));
        assert_eq!(r.ini_set("nope", b"1", &mut d), None);
        // PERDIR|SYSTEM directive is not user-settable.
        assert_eq!(r.ini_set("output_buffering", b"1", &mut d), None);
        assert_eq!(r.ini_set("html_errors", b"1", &mut d), Some(b"0".to_vec()));
        let all = r.ini_get_all(Some("core")).unwrap();
        let p = all.iter().find(|e| e.name == "precision").unwrap();
        assert_eq!(p.global_value.as_deref(), Some(&b"14"[..]));
        assert_eq!(p.local_value.as_deref(), Some(&b"6"[..]));
        assert_eq!(p.builtin_default_value.as_deref(), Some(&b"14"[..]));
        assert_eq!(p.access, 7);
        r.ini_restore("precision", &mut d);
        assert_eq!(r.get("precision"), Some(&b"14"[..]));
        assert_eq!(r.get_long("precision"), 14);
        r.deactivate(&mut d);
        assert_eq!(r.get("html_errors"), Some(&b"0"[..]));
        assert!(d.is_empty());
    }

    #[test]
    fn auto_detect_line_endings_deprecation() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(
            r.ini_set("auto_detect_line_endings", b"0", &mut d),
            Some(b"0".to_vec())
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            r.ini_set("auto_detect_line_endings", b"on", &mut d),
            Some(b"0".to_vec())
        );
        assert_eq!(r.get("auto_detect_line_endings"), Some(&b"on"[..]));
        assert!(r.get_bool("auto_detect_line_endings"));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].level, ErrorLevel::DEPRECATED);
        assert_eq!(d[0].message, "auto_detect_line_endings is deprecated");
        assert!(!d[0].docref);
        let mut startup: Vec<Diagnostic> = Vec::new();
        let _ = IniRegistry::with_core([("auto_detect_line_endings", "1")], &mut startup);
        assert!(startup
            .iter()
            .any(|x| x.message == "auto_detect_line_endings is deprecated"));
    }

    #[test]
    fn memory_limit_warnings() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(r.ini_set("memory_limit", b"1M", &mut d), None);
        assert_eq!(
            d.pop().unwrap().message,
            "Failed to set memory limit to 1048576 bytes (Current memory usage is 2097152 bytes)"
        );
        assert_eq!(r.get("memory_limit"), Some(&b"128M"[..]));
        assert_eq!(
            r.ini_set("memory_limit", b"256M", &mut d),
            Some(b"128M".to_vec())
        );
        assert_eq!(
            r.ini_set("memory_limit", b"-1", &mut d),
            Some(b"256M".to_vec())
        );
        assert_eq!(r.ini_set("memory_limit", b"12abc", &mut d), None);
        assert_eq!(
            d[0].message,
            "Invalid \"memory_limit\" setting. Invalid quantity \"12abc\": unknown multiplier \"c\", interpreting as \"12\" for backwards compatibility"
        );
        assert!(d.iter().all(|x| !x.docref));
    }

    #[test]
    fn max_memory_limit() {
        let mut d: Vec<Diagnostic> = Vec::new();
        let mut r = IniRegistry::with_core([("max_memory_limit", "256M")], &mut d);
        assert!(d.is_empty());
        assert_eq!(
            r.ini_set("memory_limit", b"512M", &mut d),
            Some(b"128M".to_vec())
        );
        assert_eq!(
            d[0].message,
            "Failed to set memory_limit to 536870912 bytes. Setting to max_memory_limit instead (currently: 268435456 bytes)"
        );
        assert_eq!(r.get("memory_limit"), Some(&b"256M"[..]));
        d.clear();
        assert_eq!(
            r.ini_set("memory_limit", b"-1", &mut d),
            Some(b"256M".to_vec())
        );
        assert!(d.is_empty());
    }

    #[test]
    fn assertions_and_display_errors() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(r.ini_set("zend.assertions", b"-1", &mut d), None);
        assert_eq!(
            d[0].message,
            "zend.assertions may be completely enabled or disabled only in php.ini"
        );
        assert_eq!(
            r.ini_set("zend.assertions", b"0", &mut d),
            Some(b"1".to_vec())
        );
        assert_eq!(
            r.ini_set("display_errors", b"stderr", &mut d),
            Some(b"1".to_vec())
        );
        assert_eq!(r.get("display_errors"), Some(&b"stderr"[..]));
        assert_eq!(r.get_long("display_errors"), 2);
        r.ini_set("display_errors", b"off", &mut d);
        assert_eq!(r.get_long("display_errors"), 0);
        r.ini_set("display_errors", b"7", &mut d);
        assert_eq!(r.get_long("display_errors"), 1);
        let mut r2 = IniRegistry::with_core([("zend.assertions", "-1")], &mut d);
        assert_eq!(r2.ini_set("zend.assertions", b"1", &mut d), None);
    }

    #[test]
    fn other_handlers() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(r.ini_set("include_path", b"", &mut d), None);
        assert_eq!(
            r.set_include_path(b"/tmp", &mut d),
            Some(b".:/usr/share/php".to_vec())
        );
        assert_eq!(r.get("include_path"), Some(&b"/tmp"[..]));
        assert_eq!(r.ini_set("arg_separator.output", b"", &mut d), None);
        assert_eq!(
            r.ini_set("zend.exception_string_param_max_len", b"1000001", &mut d),
            None
        );
        assert_eq!(
            r.ini_set("zend.exception_string_param_max_len", b"20", &mut d),
            Some(b"15".to_vec())
        );
        assert_eq!(r.ini_set("syslog.filter", b"bogus", &mut d), None);
        assert_eq!(r.ini_set("fiber.stack_size", b"-1", &mut d), None);
        assert_eq!(
            d.last().unwrap().message,
            "fiber.stack_size must be a positive number"
        );
        assert_eq!(
            r.ini_set("error_reporting", b"32767", &mut d),
            Some(Vec::new())
        );
        assert_eq!(r.get_long("error_reporting"), 32767);
        assert_eq!(r.ini_set("docref_root", b"", &mut d), Some(Vec::new()));
        assert_eq!(r.ini_set("user_agent", b"3.14", &mut d), Some(Vec::new()));
        assert_eq!(r.get("user_agent"), Some(&b"3.14"[..]));
        assert_eq!(
            r.ini_set("default_socket_timeout", b"1x", &mut d),
            Some(b"60".to_vec())
        );
        assert!(d
            .last()
            .unwrap()
            .message
            .starts_with("Invalid \"default_socket_timeout\" setting."));
    }

    #[test]
    fn get_all_sorting_and_modules() {
        let r = core();
        let all = r.ini_get_all(None).unwrap();
        let names: Vec<&str> = all.iter().map(|e| e.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_by(|a, b| strcasecmp(a, b));
        assert_eq!(names, sorted);
        // Registration order is kept by `ini_entries_registered` (same rows).
        let reg = r.ini_entries_registered(None).unwrap();
        let reg_names: Vec<&str> = reg.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            reg_names,
            r.order.iter().map(String::as_str).collect::<Vec<_>>()
        );
        assert_ne!(reg_names, names);
        assert!(r.ini_entries_registered(Some("nosuchextension")).is_err());
        // "SMTP" sorts case-insensitively between "short_open_tag" and "smtp_port".
        let i = names.iter().position(|&n| n == "SMTP").unwrap();
        assert_eq!(names[i + 1], "smtp_port");
        let e = r.ini_get_all(Some("nosuchextension")).unwrap_err();
        assert_eq!(
            e.render(Some("ini_get_all")),
            "ini_get_all(): Extension \"nosuchextension\" cannot be found"
        );
        assert!(r.ini_get_all(Some("")).is_err());
        let st = r.ini_get_all(Some("standard")).unwrap();
        assert!(st
            .iter()
            .all(|e| ["user_agent", "from"].contains(&e.name.as_str())
                || e.name.contains('_')
                || e.name.contains('.')));
    }

    #[test]
    fn displayers_follow_the_c_entry_definitions() {
        // php8.5 -n -i: "html_errors => Off => Off", "display_errors =>
        // STDOUT => STDOUT", highlight.* in <span>s even in text mode,
        // "zend.enable_gc => On", "auto_detect_line_endings => Off".
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(
            r.ini_set("html_errors", b"yes", &mut d),
            Some(b"0".to_vec())
        );
        let all = r.ini_get_all(None).unwrap();
        let shown = |name: &str, html: bool| {
            let e = all.iter().find(|e| e.name == name).unwrap();
            let (l, g) = e.displayed(html);
            (String::from_utf8(l).unwrap(), String::from_utf8(g).unwrap())
        };
        let pair = |l: &str, g: &str| (l.to_string(), g.to_string());
        assert_eq!(shown("html_errors", false), pair("On", "Off"));
        assert_eq!(shown("display_errors", false), pair("STDOUT", "STDOUT"));
        assert_eq!(shown("zend.enable_gc", false), pair("On", "On"));
        assert_eq!(shown("auto_detect_line_endings", false), pair("Off", "Off"));
        assert_eq!(shown("assert.active", false), pair("On", "On"));
        assert_eq!(
            shown("highlight.comment", false),
            pair(
                "<span style=\"color: #FF8000\">#FF8000</span>",
                "<span style=\"color: #FF8000\">#FF8000</span>"
            )
        );
        assert_eq!(shown("precision", false), pair("14", "14"));
        assert_eq!(shown("error_log", false), pair("no value", "no value"));
        assert_eq!(
            shown("error_log", true),
            pair("<i>no value</i>", "<i>no value</i>")
        );
        assert_eq!(
            shown("session.trans_sid_tags", false).0,
            "a=href,area=href,frame=src,form="
        );
        let disp = |v: &[u8]| {
            String::from_utf8(IniDisplayer::DisplayErrors.display(Some(v), false)).unwrap()
        };
        assert_eq!(disp(b"stderr"), "STDERR");
        assert_eq!(disp(b"0"), "Off");
        assert_eq!(disp(b"258"), "STDERR");
        assert_eq!(disp(b"garbage"), "Off");
        assert_eq!(disp(b"yes"), "STDOUT");
        assert_eq!(
            IniDisplayer::Default.display(Some(b"a<b>&\tc"), true),
            b"a&lt;b&gt;&amp;    c".to_vec()
        );
        assert_eq!(IniDisplayer::Boolean.display(None, false), b"Off".to_vec());
        assert_eq!(disp(b"stderr\0tail"), "Off");
        assert_eq!(
            String::from_utf8(IniDisplayer::DisplayErrors.display(None, false)).unwrap(),
            "STDOUT"
        );
        assert_eq!(
            IniDisplayer::Color.display(Some(b"#123\0x"), false),
            b"<span style=\"color: #123\">#123</span>".to_vec()
        );
    }

    #[test]
    fn entries_without_on_modify_are_not_restored_at_runtime() {
        // php8.5: ini_set('SMTP', 'x'); ini_restore('SMTP'); ini_get('SMTP')
        // is "x"; precision (OnSetPrecision) is restored.
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        for (name, old) in [("SMTP", "localhost"), ("highlight.string", "#DD0000")] {
            assert_eq!(r.ini_set(name, b"x", &mut d), Some(old.as_bytes().to_vec()));
            r.ini_restore(name, &mut d);
            assert_eq!(r.get(name), Some(&b"x"[..]));
            assert!(r.is_modified(name));
        }
        r.ini_set("precision", b"3", &mut d);
        r.ini_restore("precision", &mut d);
        assert_eq!(r.get("precision"), Some(&b"14"[..]));
        // The request end restores them all.
        r.deactivate(&mut d);
        assert_eq!(r.get("SMTP"), Some(&b"localhost"[..]));
        assert_eq!(r.get("highlight.string"), Some(&b"#DD0000"[..]));
        assert!(d.is_empty());
    }

    #[test]
    fn report_memleaks_off_is_deprecated_at_runtime() {
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(
            r.ini_set("report_memleaks", b"1", &mut d),
            Some(b"1".to_vec())
        );
        assert!(d.is_empty());
        assert_eq!(
            r.ini_set("report_memleaks", b"0", &mut d),
            Some(b"1".to_vec())
        );
        assert_eq!(d.len(), 1);
        assert_eq!(
            d[0].render(Some("ini_set")),
            "ini_set(): Directive 'report_memleaks' is deprecated"
        );
        assert!(!r.get_bool("report_memleaks"));
    }

    #[test]
    fn mail_cr_lf_mode_rejects_unknown_modes() {
        let mut d: Vec<Diagnostic> = Vec::new();
        let r = IniRegistry::with_core([("mail.cr_lf_mode".to_string(), b"abc".to_vec())], &mut d);
        assert_eq!(r.get("mail.cr_lf_mode"), Some(&b"crlf"[..]));
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].level, ErrorLevel::ERROR);
        assert_eq!(
            d[0].message,
            "Invalid value \"abc\" for mail.cr_lf_mode. Must be one of: \"crlf\", \"lf\", \"mixed\", \"os\""
        );
        let mut d: Vec<Diagnostic> = Vec::new();
        let r = IniRegistry::with_core([("mail.cr_lf_mode".to_string(), Vec::<u8>::new())], &mut d);
        assert_eq!(r.get("mail.cr_lf_mode"), Some(&b""[..]));
        assert!(d.is_empty());
    }

    #[test]
    fn memory_hard_limit_percent_is_at_least_100_or_minus_one() {
        // php8.5 -n -d memory_hard_limit_percent=50: the warning, and the
        // directive keeps its NULL default.
        let mut d: Vec<Diagnostic> = Vec::new();
        let r = IniRegistry::with_core(
            [("memory_hard_limit_percent".to_string(), b"50".to_vec())],
            &mut d,
        );
        assert_eq!(r.raw("memory_hard_limit_percent"), None);
        assert_eq!(d.len(), 1);
        assert_eq!(
            d[0].message,
            "memory_hard_limit_percent must be >= 100, or -1"
        );
        assert!(!d[0].docref);
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        assert_eq!(
            r.ini_set("memory_hard_limit_percent", b"150", &mut d),
            Some(Vec::new())
        );
        assert!(r
            .ini_set("memory_hard_limit_percent", b"-1", &mut d)
            .is_some());
        assert!(d.is_empty());
        assert!(r
            .ini_set("memory_hard_limit_percent", b"abc", &mut d)
            .is_none());
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn get_all_core_lists_every_module() {
        // php8.5 -n: ini_get_all('core') holds date.*, pcre.*, session.*, ...
        let mut r = core();
        let mut d: Vec<Diagnostic> = Vec::new();
        r.register_module("other");
        assert!(r.register(
            IniDirective::new(
                "other.x",
                Some("1"),
                Access::ALL,
                ModifyHandler::Long,
                "other"
            ),
            &mut d,
        ));
        let all = r.ini_get_all(None).unwrap();
        for name in ["core", "Core", "CORE"] {
            let c = r.ini_get_all_function(Some(name)).unwrap();
            assert_eq!(c.len(), all.len());
            assert!(c.iter().any(|e| e.name == "other.x"));
        }
        let o = r.ini_get_all_function(Some("other")).unwrap();
        assert_eq!(o.len(), 1);
        assert!(r.ini_get_all_function(Some("nope")).is_err());
        // phpinfo(), --ri/--re and ReflectionExtension match the module
        // exactly.
        let info = r.ini_get_all(Some("Core")).unwrap();
        assert!(info.iter().all(|e| e.name != "other.x"));
        let reg = r.ini_entries_registered(Some("core")).unwrap();
        assert!(reg.iter().all(|e| e.name != "other.x"));
        assert!(reg.iter().any(|e| e.name == "precision"));
    }

    #[test]
    fn sorting_persists_across_module_queries_and_registration() {
        let mut r = IniRegistry::new();
        let mut d: Vec<Diagnostic> = Vec::new();
        for (name, module) in [("zeta", "first"), ("Beta", "second"), ("alpha", "first")] {
            assert!(r.register(
                IniDirective::new(name, Some("1"), Access::ALL, ModifyHandler::Long, module),
                &mut d,
            ));
        }
        assert_eq!(r.order, ["zeta", "Beta", "alpha"]);
        assert_eq!(r.ini_set("zeta", b"2", &mut d), Some(b"1".to_vec()));
        r.sort_entries();
        assert_eq!(r.order, ["alpha", "Beta", "zeta"]);
        let first = r.ini_entries_registered(Some("first")).unwrap();
        assert_eq!(first[0].name, "alpha");
        assert_eq!(first[1].name, "zeta");
        assert_eq!(first[1].global_value, Some(b"1".to_vec()));
        assert_eq!(first[1].local_value, Some(b"2".to_vec()));
        assert_eq!(r.get_long("zeta"), 2);
        assert!(r.ini_get_all(Some("missing")).is_err());
        assert_eq!(r.order, ["alpha", "Beta", "zeta"]);
        assert!(r.register(
            IniDirective::new("aardvark", None, Access::ALL, ModifyHandler::None, "second"),
            &mut d,
        ));
        assert_eq!(r.order, ["alpha", "Beta", "zeta", "aardvark"]);
        r.sort_entries();
        assert_eq!(r.order, ["aardvark", "alpha", "Beta", "zeta"]);
        r.ini_restore("zeta", &mut d);
        assert_eq!(r.get_long("zeta"), 1);
    }

    #[test]
    fn config_values_and_activation() {
        let mut d: Vec<Diagnostic> = Vec::new();
        let mut r =
            IniRegistry::with_core([("precision", "10"), ("output_buffering", "4096")], &mut d);
        assert_eq!(r.get("precision"), Some(&b"10"[..]));
        assert_eq!(r.get_long("output_buffering"), 4096);
        assert_eq!(r.config_value("precision"), Some(&b"10"[..]));
        // A rejected configured value falls back to the default.
        let r2 = IniRegistry::with_core([("precision", "-5")], &mut d);
        assert_eq!(r2.get("precision"), Some(&b"14"[..]));
        // Per-dir style activation.
        assert!(r.alter(
            "output_buffering",
            b"1",
            Access::PERDIR,
            Stage::Activate,
            false,
            &mut d
        ));
        assert!(!r.alter(
            "enable_dl",
            b"0",
            Access::PERDIR,
            Stage::Activate,
            false,
            &mut d
        ));
        assert!(r.alter(
            "enable_dl",
            b"0",
            Access::USER,
            Stage::Runtime,
            true,
            &mut d
        ));
        assert!(!r.register(
            IniDirective::new("precision", None, Access::ALL, ModifyHandler::None, "x"),
            &mut d
        ));
    }
}
