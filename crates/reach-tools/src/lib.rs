//! Tool registry, robust child runner, SSRF guards and the obscura web backend.
//!
//! Owns everything under `~/.arana/tools/`: pinned binary installs (obscura),
//! command resolution (private copy first, then the system PATH), bounded
//! subprocess execution with Windows process-tree kill, and the URL security
//! layer (literal-IP tricks, DNS checks, redirect re-validation).
//!

pub mod config;
pub mod download;
pub mod obscura;
pub mod obscura_batch;
pub mod registry;
pub mod runner;
pub mod ssrf;

mod fsguard;

pub use obscura::{install_obscura, obscura_read, remove_obscura};
