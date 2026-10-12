//! The runner's side of the board port (ADR-0027 point 5): the HTTP adapter,
//! for a runner whose board is on a server (task 052). Solo's adapter is
//! `rimaia_core::board::InProcessBoard`, which needs no network.

pub mod http;

pub use http::HttpBoard;
