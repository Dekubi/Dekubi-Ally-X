// Adapted from victory-fork (https://github.com/fxceless/victory-fork, commit 9376fd6 + 106 VIctory fixes), a fork of
// VIctory (https://github.com/Marvisak/VIctory). GPL-3.0-or-later, by Marvisak, fxceless
// and contributors. Renamed for Dekubi X.
//! Shared plumbing for every Dekubi X crate and tool.
//!
//! * [`config`]: where the user's files live (never inside the repository).
//! * [`policy`]: the runtime half of rule #1, refusing to write
//!   game-derived data into the source tree.
//! * [`logging`]: one tracing setup for all binaries.
//! * [`notes`]: the live list of current goals.

pub mod config;
pub mod logging;
pub mod notes;
pub mod policy;

pub use config::Config;
pub use policy::{PolicyError, ensure_outside_repo};

/// Version of the Dekubi X workspace.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
