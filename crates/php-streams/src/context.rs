//! Stream contexts (`php_stream_context`).
//!
//! Options are kept as an insertion-ordered `wrapper => option => value`
//! map so `stream_context_get_options()` returns them in PHP's order. The
//! notification callback and other `params` are VM-side concerns; the VM
//! may store arbitrary parameters in [`StreamContext::params`].

use std::cell::RefCell;
use std::rc::Rc;

use crate::value::{Key, Value};

/// A stream context.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamContext {
    options: Vec<(String, Vec<(String, Value)>)>,
    /// Context parameters other than `options` (e.g. `notification`).
    pub params: Vec<(String, Value)>,
}

/// A shared context handle (contexts are resources shared by streams).
pub type ContextRef = Rc<RefCell<StreamContext>>;

impl StreamContext {
    /// An empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps a context in a shared handle.
    pub fn shared(self) -> ContextRef {
        Rc::new(RefCell::new(self))
    }

    /// `php_stream_context_get_option()`.
    pub fn get(&self, wrapper: &str, option: &str) -> Option<&Value> {
        self.options
            .iter()
            .find(|(w, _)| w == wrapper)
            .and_then(|(_, opts)| opts.iter().find(|(o, _)| o == option))
            .map(|(_, v)| v)
    }

    /// `php_stream_context_set_option()`.
    pub fn set(&mut self, wrapper: &str, option: &str, value: Value) {
        let idx = if let Some(i) = self.options.iter().position(|(w, _)| w == wrapper) {
            i
        } else {
            self.options.push((wrapper.to_string(), Vec::new()));
            self.options.len() - 1
        };
        let opts = &mut self.options[idx].1;
        if let Some(slot) = opts.iter_mut().find(|(o, _)| o == option) {
            slot.1 = value;
        } else {
            opts.push((option.to_string(), value));
        }
    }

    /// `php_stream_context_unset_option()`.
    pub fn unset(&mut self, wrapper: &str, option: &str) {
        if let Some((_, opts)) = self.options.iter_mut().find(|(w, _)| w == wrapper) {
            opts.retain(|(o, _)| o != option);
        }
    }

    /// Sets every option of a `[wrapper => [option => value]]` array, as
    /// `stream_context_create()` / `stream_context_set_option($ctx, array)`
    /// do. Returns false (PHP: `ValueError`/warning in the VM) when an entry
    /// is not an array.
    pub fn set_from_array(&mut self, options: &Value) -> bool {
        let Some(wrappers) = options.as_array() else {
            return false;
        };
        for (wkey, wval) in wrappers {
            let Some(opts) = wval.as_array() else {
                return false;
            };
            let wname = wkey.to_string();
            for (okey, oval) in opts {
                if let Key::Str(_) = okey {
                    self.set(&wname, &okey.to_string(), oval.clone());
                }
            }
        }
        true
    }

    /// All options (`stream_context_get_options()`).
    pub fn options(&self) -> &[(String, Vec<(String, Value)>)] {
        &self.options
    }

    /// All options as a PHP array value.
    pub fn options_value(&self) -> Value {
        Value::Array(
            self.options
                .iter()
                .map(|(w, opts)| {
                    (
                        Key::str(w.as_bytes()),
                        Value::Array(
                            opts.iter()
                                .map(|(o, v)| (Key::str(o.as_bytes()), v.clone()))
                                .collect(),
                        ),
                    )
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_order() {
        let mut c = StreamContext::new();
        c.set("http", "method", Value::str("POST"));
        c.set("file", "x", Value::Int(1));
        c.set("http", "header", Value::str("A: b"));
        c.set("http", "method", Value::str("GET"));
        assert_eq!(c.get("http", "method"), Some(&Value::str("GET")));
        assert_eq!(c.options()[0].1.len(), 2);
        assert_eq!(c.options()[1].0, "file");
        c.unset("http", "method");
        assert!(c.get("http", "method").is_none());
    }

    #[test]
    fn from_array() {
        let mut c = StreamContext::new();
        let v = Value::Array(vec![(
            Key::str("filter"),
            Value::Array(vec![(Key::str("max_filter_count"), Value::Int(3))]),
        )]);
        assert!(c.set_from_array(&v));
        assert_eq!(c.get("filter", "max_filter_count"), Some(&Value::Int(3)));
        assert!(!c.set_from_array(&Value::Array(vec![(Key::str("x"), Value::Int(1))])));
        assert_eq!(c.options_value().as_array().unwrap().len(), 1);
    }
}
