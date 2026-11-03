//! The safe document/node layer shared by the XML extensions.
//!
//! See [`crate::raw::tree`] for the ownership model: a [`Document`] is a
//! shared reference to an `xmlDoc`; a [`Node`] handle keeps its document
//! alive and keeps a detached node alive; attached nodes live as long as
//! their document; detached subtrees are freed with their last handle
//! (descendants that still have handles survive as detached roots).
//!
//! A tree co-owned with another object model (php-dom) goes through
//! [`SharedDocument`] ([`Document::into_shared`] /
//! [`SharedDocument::from_raw`]): the `xmlDoc` lives until the last holder
//! of either model is gone, nodes referenced by the other model
//! (`node->_private` set) are never freed here, and the other model asks
//! [`SharedDocument::is_pinned`] before freeing nodes.
//!
//! Parsing reports errors through the [`crate::error`] model:
//! [`ParseOptions::channels`] selects how PHP would have displayed them
//! when internal errors are off (`ParserChannels::CTX` for DOM-style
//! "… in Entity, line: N" warnings).

pub use crate::raw::errors::ParserChannels;
pub use crate::raw::tree::{
    Document, Namespace, Node, NodeType, ParseError, ParseOptions, SharedDocument, TreeError,
};
