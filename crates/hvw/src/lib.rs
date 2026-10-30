//! The client side of herdr-vm-worktree: the `hvw` CLI, the `hvw-watch`
//! user service and the `hvw-review` popup.
//!
//! The relay keeps one connection for each endpoint id. Thus only
//! `hvw-watch` holds the iroh endpoint of the client. `hvw` and
//! `hvw-review` reach the members through the Unix socket of `hvw-watch`
//! (see [`local`]).

pub mod git;
pub mod herdr;
pub mod local;
pub mod paths;
pub mod state;
