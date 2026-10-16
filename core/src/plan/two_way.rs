//! Two-way sync: changes flow both ways.
//!
//! When a file is missing on one side, Charon has to know whether it was deleted there or
//! newly created on the other side. So it keeps the *baseline*: the state both folders
//! agreed on after the last sync. Comparing each side with the baseline tells who changed:
//!
//! | source vs baseline | destination vs baseline | what happens                        |
//! |--------------------|-------------------------|-------------------------------------|
//! | same               | same                    | nothing                             |
//! | changed            | same                    | the source's change crosses over    |
//! | same               | changed                 | the destination's change crosses    |
//! | changed            | changed (differently)   | conflict, settled by the job's rule |
//!
//! A change can be a new file, an edit or a deletion. In a conflict between an edit and a
//! deletion, the edit wins: Charon never throws away work.

use crate::model::ConflictRule;
use crate::plan::{Action, Plan, Side};
use crate::scan::{Entry, Snapshot};
use std::collections::BTreeSet;

pub struct TwoWayPlanner {
    rule: ConflictRule,
}

impl TwoWayPlanner {
    pub fn new(rule: ConflictRule) -> Self {
        Self { rule }
    }

    /// What it takes to bring both folders together, given the last agreed state.
    pub fn plan(&self, source: &Snapshot, destination: &Snapshot, baseline: &Snapshot) -> Plan {
        let mut plan = Plan::default();
        let all_paths: BTreeSet<&String> = source.paths().chain(destination.paths()).chain(baseline.paths()).collect();

        for path in all_paths {
            let s = source.get(path);
            let d = destination.get(path);
            if Entry::same(s, d) {
                continue;
            }
            let b = baseline.get(path);
            let source_changed = !Entry::same(s, b);
            let destination_changed = !Entry::same(d, b);

            match (source_changed, destination_changed) {
                (true, false) => Self::carry(&mut plan, path, s, d, Side::Destination),
                (false, true) => Self::carry(&mut plan, path, d, s, Side::Source),
                _ => self.settle_conflict(&mut plan, path, s, d),
            }
        }
        plan
    }

    /// The baseline to remember after a run. Where both sides now agree, that's the new
    /// baseline; where they still differ (a failed copy, a stopped run, an open conflict),
    /// the old baseline stays, so the next run makes the same decision again.
    pub fn next_baseline(source: &Snapshot, destination: &Snapshot, old: &Snapshot) -> Snapshot {
        let all_paths: BTreeSet<&String> = source.paths().chain(destination.paths()).chain(old.paths()).collect();
        all_paths
            .into_iter()
            .filter_map(|path| {
                let (s, d) = (source.get(path), destination.get(path));
                let kept = if Entry::same(s, d) { s } else { old.get(path) };
                kept.map(|entry| (path.clone(), *entry))
            })
            .collect()
    }

    /// Makes side `to` match `wanted` (which comes from the other side).
    pub(crate) fn carry(plan: &mut Plan, path: &str, wanted: Option<&Entry>, existing: Option<&Entry>, to: Side) {
        let path = path.to_string();
        match wanted {
            // Deleted on the other side.
            None => {
                if let Some(existing) = existing {
                    plan.push(Action::Delete { side: to, path, dir: existing.is_dir(), whole: false });
                }
            }
            Some(wanted) => {
                if let Some(blocker) = existing.filter(|e| e.kind != wanted.kind) {
                    plan.push(Action::Delete { side: to, path: path.clone(), dir: blocker.is_dir(), whole: true });
                }
                if wanted.is_dir() {
                    plan.push(Action::CreateDir { side: to, path });
                } else {
                    let replaces = existing.is_some_and(|e| !e.is_dir());
                    plan.push(Action::Copy { to, path, bytes: wanted.size, replaces });
                }
            }
        }
    }

    /// Both sides changed the item since the last sync.
    fn settle_conflict(&self, plan: &mut Plan, path: &str, s: Option<&Entry>, d: Option<&Entry>) {
        match (s, d) {
            // Edited on one side, deleted on the other: the edit wins.
            (Some(_), None) => Self::carry(plan, path, s, d, Side::Destination),
            (None, Some(_)) => Self::carry(plan, path, d, s, Side::Source),
            (Some(src), Some(dst)) if !src.is_dir() && !dst.is_dir() => match self.rule {
                ConflictRule::NewestWins => {
                    if dst.modified > src.modified {
                        Self::carry(plan, path, d, s, Side::Source);
                    } else {
                        Self::carry(plan, path, s, d, Side::Destination);
                    }
                }
                ConflictRule::KeepBoth => plan.push(Action::KeepBoth {
                    path: path.to_string(),
                    source_bytes: src.size,
                    destination_bytes: dst.size,
                }),
                ConflictRule::Ask => Self::ask(plan, path, s, d),
            },
            // A file on one side, a folder on the other: always a question.
            _ => Self::ask(plan, path, s, d),
        }
    }

    fn ask(plan: &mut Plan, path: &str, s: Option<&Entry>, d: Option<&Entry>) {
        plan.push(Action::Conflict { path: path.to_string(), source: s.copied(), destination: d.copied() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(size: u64, modified: i64) -> Entry {
        Entry::file(size, modified)
    }

    fn snap(items: &[(&str, Entry)]) -> Snapshot {
        items.iter().map(|(p, e)| (p.to_string(), *e)).collect()
    }

    #[test]
    fn first_run_merges_both_sides() {
        let src = snap(&[("a.txt", file(1, 10_000))]);
        let dst = snap(&[("b.txt", file(2, 10_000))]);
        let plan = TwoWayPlanner::new(ConflictRule::Ask).plan(&src, &dst, &Snapshot::new());
        let s = plan.summary();
        assert_eq!((s.to_destination, s.to_source, s.deletes), (1, 1, 0));
    }

    #[test]
    fn deletion_crosses_over() {
        let base = snap(&[("a.txt", file(1, 10_000))]);
        let src = snap(&[]);
        let dst = base.clone();
        let plan = TwoWayPlanner::new(ConflictRule::Ask).plan(&src, &dst, &base);
        assert_eq!(plan.actions, vec![Action::Delete { side: Side::Destination, path: "a.txt".into(), dir: false, whole: false }]);
    }

    #[test]
    fn edit_beats_deletion() {
        let base = snap(&[("a.txt", file(1, 10_000))]);
        let src = snap(&[]);
        let dst = snap(&[("a.txt", file(5, 90_000))]);
        let plan = TwoWayPlanner::new(ConflictRule::Ask).plan(&src, &dst, &base);
        assert!(matches!(&plan.actions[0], Action::Copy { to: Side::Source, .. }));
    }

    #[test]
    fn baseline_keeps_unfinished_decisions() {
        let old = snap(&[("gone.txt", file(1, 10_000))]);
        let src = snap(&[("new.txt", file(2, 20_000))]);
        // The deletion of gone.txt on the destination failed; new.txt was copied.
        let dst = snap(&[("gone.txt", file(1, 10_000)), ("new.txt", file(2, 20_000))]);
        let next = TwoWayPlanner::next_baseline(&src, &dst, &old);
        assert!(next.get("gone.txt").is_some());
        assert!(next.get("new.txt").is_some());
        // So the next run still deletes it instead of copying it back.
        let plan = TwoWayPlanner::new(ConflictRule::Ask).plan(&src, &dst, &next);
        assert!(matches!(&plan.actions[0], Action::Delete { side: Side::Destination, .. }));
    }

    #[test]
    fn conflict_rules() {
        let base = snap(&[("a.txt", file(1, 10_000))]);
        let src = snap(&[("a.txt", file(2, 50_000))]);
        let dst = snap(&[("a.txt", file(3, 90_000))]);

        let newest = TwoWayPlanner::new(ConflictRule::NewestWins).plan(&src, &dst, &base);
        assert!(matches!(&newest.actions[0], Action::Copy { to: Side::Source, .. }));

        let both = TwoWayPlanner::new(ConflictRule::KeepBoth).plan(&src, &dst, &base);
        assert!(matches!(&both.actions[0], Action::KeepBoth { .. }));

        let ask = TwoWayPlanner::new(ConflictRule::Ask).plan(&src, &dst, &base);
        assert_eq!(ask.conflicts().len(), 1);
        assert!(ask.is_settled());
    }
}
