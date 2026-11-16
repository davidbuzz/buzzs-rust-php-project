// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! A tiny PHP interpreter (subset) for running ext/dom `.phpt` tests
//! against this crate through its generic dispatch API.

#![allow(dead_code, clippy::all, clippy::pedantic)]

pub mod builtins;
pub mod interp;
pub mod output;
pub mod value;
