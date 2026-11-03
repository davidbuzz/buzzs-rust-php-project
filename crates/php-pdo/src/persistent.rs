//! The persistent-connection list (`EG(persistent_list)` entries of type
//! "PDO persistent database"), keyed by
//! `PDO:DBH:DSN=<dsn>:<user>:<pass>[:<key>]`.

use crate::dbh::Dbh;
use crate::host::{Ctx, NullHost};
use std::collections::HashMap;
use std::rc::Rc;

/// The persistent connections of a process, owned by the host so that they
/// outlive requests.
#[derive(Default)]
pub struct PersistentList {
    entries: HashMap<Vec<u8>, Rc<Dbh>>,
}

impl std::fmt::Debug for PersistentList {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<String> = self
            .entries
            .keys()
            .map(|k| String::from_utf8_lossy(k).into_owned())
            .collect();
        keys.sort();
        f.debug_struct("PersistentList")
            .field("keys", &keys)
            .finish()
    }
}

impl PersistentList {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of cached connections.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no connection is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a connection with this hash key is cached.
    pub fn contains(&self, key: &[u8]) -> bool {
        self.entries.contains_key(key)
    }

    pub(crate) fn get(&self, key: &[u8]) -> Option<Rc<Dbh>> {
        self.entries.get(key).cloned()
    }

    pub(crate) fn insert(&mut self, key: Vec<u8>, dbh: Rc<Dbh>) {
        self.entries.insert(key, dbh);
    }

    pub(crate) fn remove(&mut self, key: &[u8]) {
        self.entries.remove(key);
    }

    /// `php_pdo_pdbh_dtor` for every entry (module shutdown): closes the
    /// cached connections.
    pub fn shutdown(&mut self) {
        let mut host = NullHost::default();
        let mut cx = Ctx::new(&mut host);
        for (_, dbh) in self.entries.drain() {
            if let Some(d) = dbh.driver.borrow_mut().take() {
                cx.with_driver(|dcx| d.close(dcx));
            }
        }
    }
}
