//! Deciding what has to cross, and which way.
//!
//! * `action.rs`  – one step (`Action`), a whole `Plan` and its `PlanSummary`
//! * `one_way.rs` – `OneWayPlanner` for Mirror and Backup
//! * `two_way.rs` – `TwoWayPlanner`, which compares both sides with the last synced state

mod action;
mod one_way;
mod two_way;

pub use action::{Action, Plan, PlanSummary, Side};
pub use one_way::OneWayPlanner;
pub use two_way::TwoWayPlanner;

use crate::model::JobSetup;
use crate::scan::Snapshot;

/// Picks the planner for the job's mode and plans the run.
pub fn plan_for(setup: &JobSetup, source: &Snapshot, destination: &Snapshot, baseline: &Snapshot) -> Plan {
    match setup.mode {
        crate::model::SyncMode::Mirror => OneWayPlanner::mirror().plan(source, destination),
        crate::model::SyncMode::Backup => OneWayPlanner::backup().plan(source, destination),
        crate::model::SyncMode::TwoWay => TwoWayPlanner::new(setup.conflict).plan(source, destination, baseline),
    }
}
