//! Carrying out a plan.
//!
//! * `executor.rs` – `Executor`: checks each step against the disk, runs it, reports progress
//! * `copier.rs`   – `Copier`: copies one file or link safely (partial file, then rename)
//! * `rename.rs`   – renaming without ever replacing what's at the new name
//! * `trash.rs`    – `Trash`: safe deletes into `.charon-trash`, and its clean-up
//! * `durable.rs`  – making a run's changes durable on the drive before they're recorded
//! * `marker.rs`   – `Marker`: tells Charon a folder is the one it synced with
//! * `meter.rs`    – `ProgressMeter`: counts what's done for the progress bar
//! * `names.rs`    – fitting new names (conflict copies, numbered trash items) to a drive's limits

mod copier;
pub(crate) mod durable;
mod executor;
mod marker;
mod meter;
mod names;
mod rename;
mod trash;

pub use copier::Copier;
pub use executor::{Care, Executor, Outcome, Shores, Spellings};
pub use marker::{Found, Marker, Markers};
pub use meter::ProgressMeter;
pub use trash::{Expired, Trash};
