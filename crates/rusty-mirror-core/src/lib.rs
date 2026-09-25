//! Shared pieces of Rusty Mirror that do not need Tauri: where things live,
//! what goes over the control socket, how frontend revisions are staged, and
//! how a consumer build proves the mirror is not in it.
//!
//! The Tauri plugin (`rusty-mirror`) and the CLI (`rusty-mirror-cli`) both
//! depend on this crate, so the two sides can never disagree about a path.

pub mod audit;
pub mod paths;
pub mod protocol;
pub mod stage;

/// Label of the hidden mirror window. Keep it out of every capability file.
pub const LABEL: &str = "rusty-mirror";
/// Query parameter carried by every mirror URL; navigation without it is refused.
pub const QUERY: &str = "rusty-mirror=1";
/// Global the frontend defines (debug builds only) to hand its state to a mirror.
pub const SNAPSHOT_GLOBAL: &str = "__RUSTY_MIRROR_SNAPSHOT__";
/// Global the mirror receives before any app script runs.
pub const SEED_GLOBAL: &str = "__RUSTY_MIRROR_SEED__";
/// Optional frontend veto consulted before `apply` reloads the primary window.
pub const BEFORE_APPLY_GLOBAL: &str = "__RUSTY_MIRROR_BEFORE_APPLY__";
/// URL prefix for staged frontend revisions served from memory.
pub const HOT_PREFIX: &str = "/__rusty_mirror/";
/// Maximum mirror lifetime. The deadline is a backstop, not a cleanup plan.
pub const MAX_SECONDS: u64 = 3600;
