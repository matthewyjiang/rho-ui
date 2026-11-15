//! A2UI Express, the compact notation a model writes a surface in: its
//! parser, its compiler to v1.0 messages, its decompiler, and the prompt
//! that teaches it.

#[expect(dead_code, reason = "the compiler, the next step, builds these errors")]
mod error;
#[expect(dead_code, reason = "the compiler, the next step, reads the tokens")]
mod lexer;
#[expect(
    dead_code,
    reason = "the compiler, the next step, reads the statements"
)]
mod parser;

pub use error::{ErrorKind, ExpressError, Position};
