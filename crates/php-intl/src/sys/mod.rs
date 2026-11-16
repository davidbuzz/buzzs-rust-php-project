// buzzs-rust-php-project - Copyright (c) 2025-2026 David Buzz <davidbuzz@gmail.com>
// Licensed under the BSD-3-Clause, the license PHP master uses; see LICENSE.
//! Thin safe wrappers over the raw [`crate::ffi`] declarations: RAII
//! handles for ICU service objects and safe functions for the stateless
//! APIs.  Every `unsafe` block in the crate lives here or in `ffi.rs` and
//! carries a `SAFETY:` comment.
#![allow(unsafe_code)]
// FFI out-parameters are passed as `&mut x` coerced to `*mut T` throughout.
#![allow(clippy::borrow_as_ptr)]

pub mod loc;
pub mod ubrk;
pub mod ucal;
pub mod uchar;
pub mod ucnv;
pub mod ucol;
pub mod udat;
pub mod udatpg;
pub mod ulistfmt;
pub mod umsg;
pub mod unorm;
pub mod unum;
pub mod unumf;
pub mod uplrules;
pub mod ureldatefmt;
pub mod ures;
pub mod usearch;
pub mod uspoof;
pub mod utext;
pub mod utrans;
