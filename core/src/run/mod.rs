//! Carrying out a plan.
//!
//! * `executor.rs` – `Executor`: runs each step and reports progress
//! * `copier.rs`   – `Copier`: copies one file safely (partial file, then rename)
//! * `trash.rs`    – `Trash`: safe deletes into `.charon-trash`, and its clean-up
//! * `meter.rs`    – `ProgressMeter`: counts what's done for the progress bar

mod copier;
mod executor;
mod meter;
mod trash;

pub use copier::Copier;
pub use executor::{Executor, Outcome, Shores};
pub use meter::ProgressMeter;
pub use trash::Trash;
