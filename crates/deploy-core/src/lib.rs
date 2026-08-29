//! Types and logic shared by `deploy-server` and the `deploy` CLI.
//!
//! Everything in here is defined once and used by both binaries. The old tool
//! kept parallel TypeScript and Rust copies of the qc parser, the RPC types and
//! the file-list rules, and they drifted; that is the drift this crate exists
//! to prevent.

pub mod config;
pub mod filelist;
pub mod glob;
pub mod hash;
pub mod qc;
pub mod rpc;
pub mod security;
pub mod sqlnames;
