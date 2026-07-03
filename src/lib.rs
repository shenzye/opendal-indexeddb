//! Unofficial OpenDAL service for browser IndexedDB on wasm targets.
//!
//! This crate provides a wasm-only [`IndexeddbBuilder`] that can be used with
//! [`opendal::Operator`] to store objects in browser IndexedDB.
//!
//! ```rust,ignore
//! // wasm32 only
//! use opendal::Operator;
//! use opendal_indexeddb::IndexeddbBuilder;
//!
//! let op = Operator::new(
//!     IndexeddbBuilder::default()
//!         .db_name("my-app")
//!         .object_store_name("files"),
//! )?
//! .finish();
//!
//! op.write("hello.txt", "hello world").await?;
//! ```
//!
//! See the README capability tables for the supported OpenDAL operations and
//! options.

#![warn(missing_docs)]

#[cfg(target_family = "wasm")]
mod backend;
/// Configuration types for the IndexedDB service.
pub mod config;
#[cfg(target_family = "wasm")]
pub use backend::IndexeddbBuilder;
pub use config::IndexeddbConfig;
#[cfg(all(test, target_family = "wasm", feature = "perf-tests"))]
mod perf;
#[cfg(all(test, target_family = "wasm"))]
mod test;
