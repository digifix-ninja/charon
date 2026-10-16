//! The data Charon keeps and the shapes the window receives.
//!
//! * `job.rs`      – a sync job and its setup (mode, conflict rule, trigger, excludes)
//! * `settings.rs` – app-wide settings
//! * `view.rs`     – what the window shows: status, progress, pending changes, summaries

mod job;
mod settings;
mod view;

pub use job::*;
pub use settings::*;
pub use view::*;
