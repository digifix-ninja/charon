//! Carrying out a plan.
//!
//! * `executor.rs` – `Executor`: checks each step against the disk, runs it, reports progress
//! * `copier.rs`   – `Copier`: copies one file or link safely (partial file, then rename)
//! * `rename.rs`   – renaming without ever replacing what's at the new name, and swapping two names
//! * `trash.rs`    – `Trash`: safe deletes into `.charon-trash`, what each run put there, and its clean-up
//! * `room.rs`     – `Room`: how much room a drive has left, so a sync never fills one up
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
pub(crate) mod rename;
mod room;
mod trash;

pub use copier::{Copier, Written};
pub use executor::{Care, Executor, Outcome, Refused, Shores, Spellings, Unread};
pub use marker::{Found, Marker, Markers};
pub use meter::ProgressMeter;
pub use room::Room;
pub use trash::{Expired, Trash, TrashLook};
