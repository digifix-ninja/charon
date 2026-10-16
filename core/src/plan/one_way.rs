//! Mirror and Backup: the destination follows the source.

use crate::plan::{Action, Plan, Side};
use crate::scan::Snapshot;
use std::collections::HashSet;

pub struct OneWayPlanner {
    /// Mirror removes what's no longer in the source; Backup never removes anything.
    mirror: bool,
}

impl OneWayPlanner {
    pub fn mirror() -> Self {
        Self { mirror: true }
    }

    pub fn backup() -> Self {
        Self { mirror: false }
    }

    /// What it takes for `destination` to follow `source`.
    pub fn plan(&self, source: &Snapshot, destination: &Snapshot) -> Plan {
        let mut plan = Plan::default();
        for (path, wanted) in source.iter() {
            let existing = destination.get(path);
            if existing.is_some_and(|e| e.same_as(wanted)) {
                continue;
            }
            // Something of the other kind is in the way (a file where a folder goes, or the
            // reverse): it has to go first.
            let in_the_way = existing.filter(|e| e.kind != wanted.kind);
            if let Some(blocker) = in_the_way {
                plan.push(Action::Delete { side: Side::Destination, path: path.clone(), dir: blocker.is_dir(), whole: true });
            }
            if wanted.is_dir() {
                plan.push(Action::CreateDir { side: Side::Destination, path: path.clone() });
            } else {
                plan.push(Action::Copy {
                    to: Side::Destination,
                    path: path.clone(),
                    bytes: wanted.size,
                    replaces: existing.is_some() && in_the_way.is_none(),
                });
            }
        }
        if self.mirror {
            self.plan_removals(source, destination, &mut plan);
        }
        plan
    }

    /// Mirror: whatever the source no longer has. A removed folder takes its contents along,
    /// so nothing inside it is listed separately.
    fn plan_removals(&self, source: &Snapshot, destination: &Snapshot, plan: &mut Plan) {
        let mut removed_folders: HashSet<&str> = HashSet::new();
        for (path, entry) in destination.iter() {
            if source.get(path).is_some() || Self::inside_any(path, &removed_folders) {
                continue;
            }
            if entry.is_dir() {
                removed_folders.insert(path);
            }
            plan.push(Action::Delete { side: Side::Destination, path: path.clone(), dir: entry.is_dir(), whole: true });
        }
    }

    /// Whether one of `path`'s parent folders is in `folders`.
    fn inside_any(path: &str, folders: &HashSet<&str>) -> bool {
        path.match_indices('/').any(|(i, _)| folders.contains(&path[..i]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Entry;

    fn snap(items: &[(&str, Option<(u64, i64)>)]) -> Snapshot {
        items
            .iter()
            .map(|(p, f)| (p.to_string(), f.map_or(Entry::dir(), |(s, m)| Entry::file(s, m))))
            .collect()
    }

    #[test]
    fn copies_new_and_changed_files() {
        let src = snap(&[("a.txt", Some((1, 1000))), ("b.txt", Some((2, 9000))), ("docs", None), ("docs/c.txt", Some((3, 1000)))]);
        let dst = snap(&[("a.txt", Some((1, 1500))), ("b.txt", Some((2, 1000)))]);
        let plan = OneWayPlanner::backup().plan(&src, &dst);
        let s = plan.summary();
        assert_eq!(s.copies, 2); // b changed, c new; a is within the time tolerance
        assert_eq!(s.new_folders, 1);
        assert_eq!(s.deletes, 0);
    }

    #[test]
    fn backup_never_deletes() {
        let src = snap(&[]);
        let dst = snap(&[("old.txt", Some((1, 1)))]);
        assert!(OneWayPlanner::backup().plan(&src, &dst).is_settled());
    }

    #[test]
    fn mirror_removes_folders_whole() {
        let src = snap(&[("keep.txt", Some((1, 1)))]);
        let dst = snap(&[("keep.txt", Some((1, 1))), ("gone", None), ("gone/a.txt", Some((1, 1))), ("gone-too.txt", Some((1, 1)))]);
        let plan = OneWayPlanner::mirror().plan(&src, &dst);
        let paths: Vec<&str> = plan.actions.iter().map(|a| a.path()).collect();
        assert_eq!(paths, vec!["gone", "gone-too.txt"]);
    }

    #[test]
    fn replaces_items_of_the_other_kind() {
        let src = snap(&[("x", Some((1, 1)))]);
        let dst = snap(&[("x", None), ("x/inner.txt", Some((1, 1)))]);
        let plan = OneWayPlanner::mirror().plan(&src, &dst);
        let ordered = plan.ordered();
        assert!(matches!(ordered[0], Action::Delete { path, whole: true, .. } if path == "x"));
        assert!(matches!(ordered.last().unwrap(), Action::Copy { replaces: false, .. }));
    }
}
