//! A tiny PHP interpreter (subset) for running ext/dom `.phpt` tests
//! against this crate through its generic dispatch API.

#![allow(dead_code, clippy::all, clippy::pedantic)]

pub mod builtins;
pub mod interp;
pub mod output;
pub mod value;
