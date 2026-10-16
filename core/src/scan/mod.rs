//! Reading a folder into a snapshot.
//!
//! * `entry.rs`   – one file or folder (`Entry`) and a whole folder (`Snapshot`)
//! * `filter.rs`  – exclude patterns (`ExcludeFilter`)
//! * `scanner.rs` – walking a folder (`Scanner`)

mod entry;
mod filter;
mod scanner;

pub use entry::{Entry, EntryKind, Snapshot, MTIME_TOLERANCE_MS};
pub use filter::{ExcludeFilter, PARTIAL_SUFFIX, TRASH_FOLDER};
pub use scanner::Scanner;
