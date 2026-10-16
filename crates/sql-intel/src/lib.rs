//! SQL tokenization, syntax highlighting, and autocomplete, shared across
//! every backend so the `app` crate's editor behaves identically whether
//! it's connected to Postgres, MySQL, or SQL Server (modulo each
//! dialect's own keywords/quoting, picked via [`dialect::dialect_for`]).

pub mod complete;
pub mod dialect;
pub mod highlight;

pub use complete::{complete, Suggestion, SuggestionKind};
pub use dialect::{dialect_for, generic_dialect};
pub use highlight::{highlight, HighlightSpan, TokenKind};
