//! The safety checks every plan goes through before it touches anything.
//!
//! * A **refusal** stops the run in every mode: one side is empty while the plan would
//!   delete on the other (almost always an unmounted drive or a wrong folder).
//! * A **concern** is a plan that looks like something went wrong: it removes or replaces a
//!   large part of one side, empties files, or follows a folder that suddenly looks empty.
//!   A run nobody looked at (full automation) stops before it starts and waits for the
//!   person's OK; a review shows the concern in red and asks for an explicit tick.

use crate::model::{human_count, plural};
use crate::plan::{Action, Baseline, Plan, Side};
use crate::scan::{Keys, Snapshot};
use std::collections::BTreeSet;

/// Removals that need a look: at least this many files on one side…
const FEW: u64 = 3;
/// …that are at least half of the files there.
const MOST: f64 = 0.5;
/// Removals and replacements that need a look: at least this many files on one side that
/// are at least half of the files there (a small folder rewritten by a broken tool)…
const FEW_DESTRUCTIVE: u64 = 5;
/// Removals and replacements that need a look: more than this many files on one side…
const MANY: u64 = 20;
/// …that are more than this share of the files there, or…
const MASS_SHARE: f64 = 0.25;
/// …more than this many in any case.
const MASS_MAX: u64 = 1_000;
/// A folder that held at least this many items and now looks empty needs a look.
const EMPTIED_FOLDER: usize = 5;
/// At most this many names are listed in one concern.
const LISTED: usize = 3;

/// Files and links removed or replaced by automatic runs during the last day, per side (by
/// key: a file replaced again and again counts once).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recent {
    pub source: BTreeSet<String>,
    pub destination: BTreeSet<String>,
}

impl Recent {
    fn on(&self, side: Side) -> &BTreeSet<String> {
        match side {
            Side::Source => &self.source,
            Side::Destination => &self.destination,
        }
    }
}

pub struct Guard;

impl Guard {
    /// Why this plan must not run at all, if it mustn't.
    pub fn refusal(plan: &Plan, source: &Snapshot, destination: &Snapshot) -> Option<String> {
        let deletes_on = |side: Side| plan.actions.iter().any(|a| matches!(a, Action::Delete { side: s, .. } if *s == side));
        if source.file_count() == 0 && destination.file_count() > 0 && deletes_on(Side::Destination) {
            return Some(
                "The source folder is empty, so this sync would remove everything in the destination. \
                 Charon won't do that: it's almost always a drive that isn't connected properly or a wrong folder. \
                 If you really want an empty destination, empty it yourself."
                    .into(),
            );
        }
        if destination.file_count() == 0 && source.file_count() > 0 && deletes_on(Side::Source) {
            return Some(
                "The destination folder is empty, so this sync would remove everything in the source. \
                 Charon won't do that: it's almost always a drive that isn't connected properly or a wrong folder."
                    .into(),
            );
        }
        None
    }

    /// Everything about this plan that deserves a careful look, in words. `recent` is given
    /// for jobs with full automation: what automatic runs already removed or replaced today
    /// counts too, so damage that spreads slowly is caught as well.
    pub fn concerns(plan: &Plan, source: &Snapshot, destination: &Snapshot, baseline: &Baseline, recent: Option<&Recent>) -> Vec<String> {
        let mut concerns = Vec::new();
        for (side, snapshot, other) in [(Side::Source, source, destination), (Side::Destination, destination, source)] {
            let total = snapshot.file_count() as u64;
            let removed = plan.removed_on(side);
            let destructive = plan.destructive_on(side);
            let this_run = if Self::most(removed, total) {
                Some(format!("This sync would remove {} of the {} files in the {}.", human_count(removed), human_count(total), side.word()))
            } else if Self::mass(destructive, total) || Self::most_destructive(destructive, total) {
                Some(format!(
                    "This sync would remove or replace {} of the {} files in the {}.",
                    human_count(destructive),
                    human_count(total),
                    side.word()
                ))
            } else {
                None
            };
            let within_a_day = match (&this_run, recent) {
                (None, Some(recent)) if destructive > 0 => Self::within_a_day(plan, snapshot, recent.on(side), side),
                _ => None,
            };
            concerns.extend(this_run.into_iter().chain(within_a_day));
            if let Some(concern) = Self::emptied_files(plan, side) {
                concerns.push(concern);
            }
            if let Some(concern) = Self::emptied_folders(plan, other, baseline, side) {
                concerns.push(concern);
            }
        }
        concerns
    }

    /// At least a few files, and at least half of them.
    fn most(hit: u64, total: u64) -> bool {
        hit >= FEW && hit as f64 >= total as f64 * MOST
    }

    /// Many files and a good share of them, or very many.
    fn mass(hit: u64, total: u64) -> bool {
        (hit > MANY && hit as f64 > total as f64 * MASS_SHARE) || hit > MASS_MAX
    }

    /// Several files that are at least half of them.
    fn most_destructive(hit: u64, total: u64) -> bool {
        hit >= FEW_DESTRUCTIVE && hit as f64 >= total as f64 * MOST
    }

    /// The files this plan and the automatic runs of the last day (`earlier`) removed or
    /// replaced on `side` together, each counted once, if that's too many.
    fn within_a_day(plan: &Plan, snapshot: &Snapshot, earlier: &BTreeSet<String>, side: Side) -> Option<String> {
        let today: BTreeSet<&str> = earlier.iter().map(String::as_str).chain(plan.destructive_paths_on(side)).collect();
        // The files there at the start of the day: those there now, and those removed since.
        let gone = earlier.iter().filter(|key| !snapshot.contains(key)).count() as u64;
        let (today, total) = (today.len() as u64, snapshot.file_count() as u64 + gone);
        (Self::mass(today, total) || Self::most_destructive(today, total)).then(|| {
            format!(
                "Automatic syncs and this one would remove or replace {} of the {} files in the {} within a day.",
                human_count(today),
                human_count(total),
                side.word()
            )
        })
    }

    /// Files with contents that would be replaced by empty files: a crashed or broken app
    /// often leaves those behind.
    fn emptied_files(plan: &Plan, side: Side) -> Option<String> {
        let emptied: Vec<&str> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Copy { to, path, from, existing: Some(existing), .. }
                    if *to == side && from.is_file() && from.size == 0 && existing.is_file() && existing.size > 0 =>
                {
                    Some(path.as_str())
                }
                _ => None,
            })
            .collect();
        (!emptied.is_empty()).then(|| {
            format!(
                "{} in the {} would be replaced by {}: {}.",
                Self::subject(emptied.len(), "file"),
                side.word(),
                if emptied.len() == 1 { "an empty file" } else { "empty files" },
                Self::list(&emptied)
            )
        })
    }

    /// Folders on the other side (`other`) that are there but look empty, although they held
    /// several items at the last sync, while the plan carries that emptiness over to `side`.
    /// A share or a cloud folder that lists a folder before it has loaded looks exactly like that.
    fn emptied_folders(plan: &Plan, other: &Snapshot, baseline: &Baseline, side: Side) -> Option<String> {
        let candidates: BTreeSet<&str> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Delete { side: s, path, .. } if *s == side => Some(path.as_str()),
                _ => None,
            })
            .flat_map(Keys::ancestors)
            .collect();
        let emptied: Vec<&str> = candidates
            .into_iter()
            .filter(|folder| {
                let looks_empty = other.get(folder).is_some_and(|e| e.is_dir()) && other.inside(folder).next().is_none();
                looks_empty && baseline.held_at_least(folder, EMPTIED_FOLDER)
            })
            .collect();
        // Only the topmost of nested ones.
        let folders: Vec<&str> =
            emptied.iter().copied().filter(|f| !emptied.iter().any(|top| top != f && Keys::is_within(f, top))).collect();
        (!folders.is_empty()).then(|| {
            format!(
                "{} on the {} now {} empty, though {} several items at the last sync: {}.",
                Self::subject(folders.len(), "folder"),
                side.opposite().word(),
                if folders.len() == 1 { "looks" } else { "look" },
                if folders.len() == 1 { "it held" } else { "they held" },
                Self::list(&folders)
            )
        })
    }

    /// "1 file", "3 files" at the start of a sentence.
    fn subject(n: usize, word: &str) -> String {
        let text = plural(n as u64, word);
        let mut chars = text.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().chain(chars).collect(),
            None => text,
        }
    }

    /// "a, b, c and 4 more".
    fn list(names: &[&str]) -> String {
        let shown: Vec<&str> = names.iter().take(LISTED).copied().collect();
        if names.len() > LISTED {
            format!("{} and {} more", shown.join(", "), human_count((names.len() - LISTED) as u64))
        } else {
            shown.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Pair;
    use crate::scan::Entry;

    fn files(n: usize) -> Snapshot {
        (0..n).map(|i| (format!("f{i}.txt"), Entry::file(1, 10_000))).collect()
    }

    fn deleting(n: usize) -> Plan {
        let mut plan = Plan::default();
        for i in 0..n {
            plan.push(Action::delete(Side::Destination, &format!("f{i}.txt"), Entry::file(1, 10_000), None));
        }
        plan
    }

    fn concerns(plan: &Plan, source: &Snapshot, destination: &Snapshot) -> Vec<String> {
        Guard::concerns(plan, source, destination, &Baseline::new(), None)
    }

    #[test]
    fn an_empty_side_is_refused_even_with_one_file_left_elsewhere() {
        assert!(Guard::refusal(&deleting(3), &files(0), &files(3)).is_some());
        assert!(Guard::refusal(&deleting(3), &files(1), &files(4)).is_none(), "that's for the concerns to catch");
        assert!(!concerns(&deleting(3), &files(1), &files(4)).is_empty());
    }

    #[test]
    fn small_folders_are_protected_too() {
        // 18 of 19 contracts: no "more than 20" rule would see that.
        assert!(!concerns(&deleting(18), &files(1), &files(19)).is_empty());
        // Two removals are routine.
        assert!(concerns(&deleting(2), &files(2), &files(4)).is_empty());
    }

    #[test]
    fn large_removals_need_a_look() {
        assert!(!concerns(&deleting(99), &files(1), &files(100)).is_empty());
        assert!(concerns(&deleting(5), &files(95), &files(100)).is_empty());
        // 21 of 10,000 is routine; 1,001 of 10,000 isn't.
        assert!(concerns(&deleting(21), &files(9_979), &files(10_000)).is_empty());
        assert!(!concerns(&deleting(1_001), &files(8_999), &files(10_000)).is_empty());
    }

    fn replacing(names: impl IntoIterator<Item = String>) -> Plan {
        let mut plan = Plan::default();
        for name in names {
            plan.push(Action::copy(Side::Destination, &name, Entry::file(2, 20_000), Some(Entry::file(1, 10_000))));
        }
        plan
    }

    fn earlier(names: impl IntoIterator<Item = String>) -> Recent {
        Recent { source: BTreeSet::new(), destination: names.into_iter().collect() }
    }

    fn with_recent(plan: &Plan, source: &Snapshot, destination: &Snapshot, recent: &Recent) -> Vec<String> {
        Guard::concerns(plan, source, destination, &Baseline::new(), Some(recent))
    }

    #[test]
    fn slow_damage_adds_up_over_a_day() {
        let plan = deleting(10);
        let (source, destination) = (files(990), files(1_000));
        assert!(with_recent(&plan, &source, &destination, &Recent::default()).is_empty());
        let busy_day = earlier((100..400).map(|i| format!("f{i}.txt")));
        assert!(!with_recent(&plan, &source, &destination, &busy_day).is_empty());
    }

    #[test]
    fn a_small_folder_rewritten_in_place_needs_a_look() {
        // 19 contracts encrypted in place by ransomware: no removal, no empty file.
        let all = replacing((0..19).map(|i| format!("f{i}.txt")));
        assert!(!concerns(&all, &files(19), &files(19)).is_empty());
        // 4 of 19 edited is ordinary work.
        let few = replacing((0..4).map(|i| format!("f{i}.txt")));
        assert!(concerns(&few, &files(19), &files(19)).is_empty());
    }

    #[test]
    fn a_small_folder_damaged_over_several_runs_needs_a_look() {
        // 15 replaced earlier today, 4 now.
        let plan = replacing((15..19).map(|i| format!("f{i}.txt")));
        let recent = earlier((0..15).map(|i| format!("f{i}.txt")));
        assert!(!with_recent(&plan, &files(19), &files(19), &recent).is_empty());
        // 2 removed now and 4 earlier today, of 6.
        let recent = earlier((2..6).map(|i| format!("f{i}.txt")));
        assert!(!with_recent(&deleting(2), &files(2), &files(2), &recent).is_empty());
    }

    #[test]
    fn a_file_saved_again_and_again_counts_once() {
        // A log replaced in every run of the day, in a folder of 80.
        let recent = earlier(["f0.txt".to_string()]);
        let plan = replacing(["f0.txt".to_string()]);
        assert!(with_recent(&plan, &files(80), &files(80), &recent).is_empty());
        // 21 different files of 80 aren't ordinary.
        let recent = earlier((1..21).map(|i| format!("f{i}.txt")));
        assert!(!with_recent(&plan, &files(80), &files(80), &recent).is_empty());
    }

    #[test]
    fn files_emptied_by_a_broken_app_need_a_look() {
        let mut plan = Plan::default();
        plan.push(Action::copy(Side::Destination, "report.docx", Entry::file(0, 20_000), Some(Entry::file(52_000, 10_000))));
        let found = concerns(&plan, &files(100), &files(100));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("report.docx"));
    }

    #[test]
    fn a_folder_that_suddenly_looks_empty_needs_a_look() {
        let mut baseline = Baseline::new();
        let mut plan = Plan::default();
        baseline.insert("contracts", Pair::new(Entry::dir(), Entry::dir()));
        for i in 0..6 {
            let path = format!("contracts/c{i}.pdf");
            baseline.insert(path.clone(), Pair::new(Entry::file(1, 1), Entry::file(1, 1)));
            plan.push(Action::delete(Side::Source, &path, Entry::file(1, 1), None));
        }
        let mut destination = files(1_000);
        destination.insert("contracts", Entry::dir());
        let found = Guard::concerns(&plan, &files(1_000), &destination, &baseline, None);
        assert!(found.iter().any(|c| c.contains("contracts") && c.contains("destination")), "{found:?}");
    }
}
