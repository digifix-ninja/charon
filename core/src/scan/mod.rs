//! Reading a folder into a snapshot.
//!
//! * `entry.rs`   – one file, folder or link (`Entry`) and a whole folder (`Snapshot`)
//! * `contents.rs` – comparing two files byte for byte (`Contents`)
//! * `filter.rs`  – exclude patterns (`ExcludeFilter`) and system clutter
//! * `keys.rs`    – paths as Charon compares them (`Keys`: NFC, `/`-separated)
//! * `root.rs`    – which folder a path really is (`RootId`: drive and folder number)
//! * `scanner.rs` – walking a folder (`Scanner`), noting every place it couldn't read
//! * `volume.rs`  – what kind of drive a folder is on and what it can hold (`Volume`)

mod contents;
mod entry;
mod filter;
mod keys;
mod root;
mod scanner;
mod volume;

pub use contents::Contents;
pub use entry::{Entry, EntryKind, ScanFacts, Skipped, Snapshot, MTIME_TOLERANCE_MS};
pub use filter::{
    is_clutter, is_legacy_partial, is_partial, is_system_folder, partial_started, ExcludeFilter, MARKER_FOLDER, PARTIAL_EXTENSION, PARTIAL_PREFIX, PARTIAL_SUFFIX,
    TRASH_FOLDER,
};
pub use keys::Keys;
pub use root::RootId;
pub use scanner::{Scanner, PRIVACY_REFUSAL};
pub use volume::{never_download_on_this_thread, FsKind, Refusal, Volume};
