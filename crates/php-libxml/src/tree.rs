//! The safe document/node layer shared by the XML extensions.
//!
//! See [`crate::raw::tree`] for the ownership model: a [`Document`] is a
//! shared reference to an `xmlDoc`; a [`Node`] handle keeps its document
//! alive and keeps a detached node alive; attached nodes live as long as
//! their document; detached subtrees are freed with their last handle
//! (descendants that still have handles survive as detached roots).
//!
//! Parsing reports errors through the [`crate::error`] model:
//! [`ParseOptions::channels`] selects how PHP would have displayed them
//! when internal errors are off (`ParserChannels::CTX` for DOM-style
//! "… in Entity, line: N" warnings).

pub use crate::raw::errors::ParserChannels;
pub use crate::raw::tree::{
    Document, Namespace, Node, NodeType, ParseError, ParseOptions, TreeError,
};
