//! Settling one two-way conflict the way the person chose: keep the source's version, keep
//! the destination's, or keep both files.
//!
//! The plan only touches the conflict's own item (and, for a folder, what's inside it), and
//! goes through the same review and the same checks as any sync. Places Charon can't read
//! or must leave alone stay as they are, with a note.

use crate::model::ConflictChoice;
use crate::plan::ground::Ground;
use crate::plan::{Action, Plan, Side};
use crate::scan::{Entry, Keys, Snapshot};

pub struct Resolution;

impl Resolution {
    /// The steps that settle the conflict at `key` by `choice`, or why it can't be done.
    pub fn plan(source: &Snapshot, destination: &Snapshot, key: &str, choice: ConflictChoice) -> Result<Plan, String> {
        let mut everything = Plan::default();
        let mut ground = Ground::new(source, destination, &[Side::Source, Side::Destination], &mut everything);
        // Only the notes about this item and what's inside it.
        let notes = everything
            .notes
            .into_iter()
            .filter(|note| {
                let snapshot = match note.side {
                    Some(Side::Destination) => destination,
                    _ => source,
                };
                let at = snapshot.key_for(&note.path);
                Keys::is_within(&at, key) || Keys::is_within(key, &at)
            })
            .collect();
        let mut plan = Plan { notes, ..Plan::default() };
        if ground.is_off_limits(key) {
            return Err("Charon can't settle this item: it's in a place it leaves alone (see the notes).".into());
        }
        let (s, d) = (source.get(key), destination.get(key));
        match choice {
            ConflictChoice::KeepSource => Self::keep(&mut plan, &mut ground, key, Side::Source)?,
            ConflictChoice::KeepDestination => Self::keep(&mut plan, &mut ground, key, Side::Destination)?,
            ConflictChoice::KeepBoth => match (s, d) {
                (Some(src), Some(dst)) if src.is_file() && dst.is_file() => plan.push(Action::KeepBoth {
                    path: key.to_string(),
                    source_bytes: src.size,
                    destination_bytes: dst.size,
                    source: *src,
                    destination: *dst,
                }),
                _ => return Err("Only two files can both be kept. Choose one side's version.".into()),
            },
        }
        ground.finish(&mut plan);
        // A folder that holds items Charon leaves alone can't make way: settling the item
        // halfway would only empty part of that folder.
        if choice != ConflictChoice::KeepBoth {
            let winner = if choice == ConflictChoice::KeepSource { Side::Source } else { Side::Destination };
            let loser = winner.opposite();
            let wanted = ground.side(winner).get(key);
            let settled = plan.actions.iter().any(|action| match action {
                Action::Copy { to, path, .. } | Action::CreateDir { side: to, path } => wanted.is_some() && *to == loser && path == key,
                Action::Delete { side, path, .. } => wanted.is_none() && *side == loser && path == key,
                _ => false,
            });
            if !settled {
                return Err(format!(
                    "The {} has a folder “{}” holding items Charon leaves alone (excluded or unreadable), so it can't be replaced. Move them out of it first, or choose the other version.",
                    loser.word(),
                    ground.side(loser).spelling(key)
                ));
            }
        }
        Ok(plan)
    }

    /// Makes the other side match `winner`'s version of `key`.
    fn keep(plan: &mut Plan, ground: &mut Ground, key: &str, winner: Side) -> Result<(), String> {
        let loser = winner.opposite();
        let (from, here) = (ground.side(winner), ground.side(loser));
        match (from.get(key), here.get(key)) {
            (None, None) => Err("Neither side has this item anymore. Look again.".into()),
            (None, Some(existing)) => {
                Self::remove(plan, ground, loser, key, *existing, None);
                Ok(())
            }
            (Some(wanted), None) => {
                Self::bring(plan, ground, loser, key, *wanted);
                Ok(())
            }
            (Some(wanted), Some(existing)) if wanted.kind == existing.kind => {
                if wanted.is_dir() {
                    return Err("Both sides have a folder here, so there's nothing to settle. Look again.".into());
                }
                plan.push(Action::copy(loser, key, *wanted, Some(*existing)).settling(true));
                Ok(())
            }
            (Some(wanted), Some(existing)) => {
                Self::remove(plan, ground, loser, key, *existing, Some(*wanted));
                Self::bring(plan, ground, loser, key, *wanted);
                Ok(())
            }
        }
    }

    /// Removes `key` from `side`, with everything inside it. `instead`: what takes its place.
    fn remove(plan: &mut Plan, ground: &Ground, side: Side, key: &str, entry: Entry, instead: Option<Entry>) {
        let snapshot = ground.side(side);
        let inside: Vec<(String, Entry)> = snapshot.inside(key).map(|(path, entry)| (path.clone(), *entry)).collect();
        for (path, entry) in inside {
            if !ground.is_off_limits(&path) {
                plan.push(Action::delete(side, &path, entry, None));
            }
        }
        plan.push(Action::delete(side, key, entry, instead));
    }

    /// Creates `key` on `to` as the other side has it, with everything inside it.
    fn bring(plan: &mut Plan, ground: &Ground, to: Side, key: &str, wanted: Entry) {
        let from = ground.side(to.opposite());
        Self::bring_one(plan, to, key, &wanted);
        if wanted.is_dir() {
            let inside: Vec<(String, Entry)> = from.inside(key).map(|(path, entry)| (path.clone(), *entry)).collect();
            for (path, entry) in inside {
                if !ground.is_off_limits(&path) {
                    Self::bring_one(plan, to, &path, &entry);
                }
            }
        }
    }

    fn bring_one(plan: &mut Plan, to: Side, key: &str, wanted: &Entry) {
        if wanted.is_dir() {
            plan.push(Action::CreateDir { side: to, path: key.to_string() });
        } else {
            plan.push(Action::copy(to, key, *wanted, None));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(items: &[(&str, Entry)]) -> Snapshot {
        items.iter().map(|(p, e)| (p.to_string(), *e)).collect()
    }

    #[test]
    fn keeping_one_version_replaces_the_other_as_a_settlement() {
        let (s, d) = (Entry::file(1, 10_000), Entry::file(2, 20_000));
        let plan = Resolution::plan(&snap(&[("a.txt", s)]), &snap(&[("a.txt", d)]), "a.txt", ConflictChoice::KeepSource).unwrap();
        assert_eq!(plan.actions, vec![Action::copy(Side::Destination, "a.txt", s, Some(d)).settling(true)]);
    }

    #[test]
    fn a_folder_meeting_a_file_is_settled_item_by_item() {
        let source = snap(&[("x", Entry::file(3, 1))]);
        let destination = snap(&[("x", Entry::dir()), ("x/a.txt", Entry::file(1, 1)), ("x/b.txt", Entry::file(1, 1))]);
        let plan = Resolution::plan(&source, &destination, "x", ConflictChoice::KeepSource).unwrap();
        let ordered: Vec<String> = plan.ordered().iter().map(|a| format!("{:?}", std::mem::discriminant(*a)) + a.path()).collect();
        assert_eq!(plan.actions.len(), 4, "{:?}", plan.actions);
        // The folder's contents and the folder go first, then the file takes its place.
        assert!(ordered.last().unwrap().ends_with('x'));
        assert!(matches!(plan.ordered().last(), Some(Action::Copy { .. })));
    }

    #[test]
    fn only_two_files_can_both_be_kept() {
        let source = snap(&[("x", Entry::file(3, 1))]);
        let destination = snap(&[("x", Entry::dir())]);
        assert!(Resolution::plan(&source, &destination, "x", ConflictChoice::KeepBoth).is_err());
    }

    #[test]
    fn a_folder_holding_unreadable_places_is_never_half_emptied() {
        let source = snap(&[]);
        let mut destination = snap(&[("x", Entry::dir()), ("x/locked", Entry::dir()), ("x/a.txt", Entry::file(1, 1))]);
        destination.freeze("x/locked", "permission denied");
        let refused = Resolution::plan(&source, &destination, "x", ConflictChoice::KeepSource).unwrap_err();
        assert!(refused.contains("leaves alone"), "{refused}");
        // The other way round touches nothing locked: it's fine.
        assert!(Resolution::plan(&source, &destination, "x", ConflictChoice::KeepDestination).is_ok());
    }

    #[test]
    fn a_folder_holding_excluded_items_is_never_half_emptied() {
        let source = snap(&[("x", Entry::file(3, 1))]);
        let mut destination = snap(&[("x", Entry::dir()), ("x/a.txt", Entry::file(1, 1)), ("x/b.txt", Entry::file(1, 1))]);
        destination.occupy("x"); // cache.tmp, excluded, is in there too
        let refused = Resolution::plan(&source, &destination, "x", ConflictChoice::KeepSource).unwrap_err();
        assert!(refused.contains("“x”"), "{refused}");
    }
}
