//! What both planners need besides the items themselves: the places they must not touch,
//! and a last look at folder removals once every step is known.

use crate::plan::{Action, Baseline, NoteKind, Plan, Side};
use crate::scan::{Entry, EntryKind, Keys, Snapshot, Volume};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use unicode_normalization::UnicodeNormalization;

pub(crate) struct Ground<'a> {
    source: &'a Snapshot,
    destination: &'a Snapshot,
    /// Places nothing may touch on either side: unreadable on one side, names written two
    /// ways, names one side leaves out, items the receiving drive can't hold. Everything
    /// inside them is off-limits too.
    frozen: BTreeSet<String>,
    /// Places a planning decision put off-limits (a file meeting a folder), with everything inside.
    blocked: BTreeSet<String>,
}

impl<'a> Ground<'a> {
    /// The ground for a plan that may copy onto the sides in `receiving`, with a note in
    /// `plan` for every place left alone.
    pub fn new(source: &'a Snapshot, destination: &'a Snapshot, receiving: &[Side], plan: &mut Plan) -> Self {
        let mut ground = Self { source, destination, frozen: BTreeSet::new(), blocked: BTreeSet::new() };
        for side in [Side::Source, Side::Destination] {
            let snapshot = ground.side(side);
            for (key, reason) in snapshot.frozen_places() {
                let kind = if snapshot.is_clash(key) { NoteKind::NameClash } else { NoteKind::Unreadable };
                plan.note(kind, Some(side), snapshot.spelling(key), reason.clone());
                ground.frozen.insert(key.clone());
            }
            for skipped in snapshot.skipped() {
                plan.note(NoteKind::LeftOut, Some(side), skipped.path.clone(), skipped.reason.clone());
                // Its name is left alone on both sides, under this one note: a copy into its
                // place would fail on every sync and hold back every removal on that side.
                if let Some(key) = &skipped.key {
                    ground.frozen.insert(key.clone());
                }
            }
        }
        for (side, key, reason) in ground.taken_for_reserved() {
            plan.note(NoteKind::LeftOut, Some(side), ground.side(side).spelling(&key), reason);
            ground.frozen.insert(key);
        }
        for &to in receiving {
            for (key, reason) in ground.unholdable(to) {
                let from = ground.side(to.opposite());
                plan.note(NoteKind::LeftOut, Some(to.opposite()), from.spelling(&key), reason);
                ground.frozen.insert(key);
            }
        }
        ground
    }

    /// The same ground, without notes (for working out a baseline).
    pub fn quiet(source: &'a Snapshot, destination: &'a Snapshot, receiving: &[Side]) -> Self {
        Self::new(source, destination, receiving, &mut Plan::default())
    }

    /// Items on either side whose key is a name one of the sides keeps for Charon's or the
    /// drive's own use (see `Snapshot::reserved`), with the side they are on and the reason
    /// they are left alone. Such names are matched as they are written, so the same name
    /// written another way has the key of the one kept, and a drive that ignores case would
    /// take it for that one. The scans already leave out the names of Charon's folders and
    /// the drive's in other upper and lower case; this catches any other spelling of a name
    /// one side keeps.
    fn taken_for_reserved(&self) -> Vec<(Side, String, String)> {
        let mut reserved: BTreeMap<&String, (Side, &String)> = BTreeMap::new();
        for side in [Side::Source, Side::Destination] {
            for (key, disk) in self.side(side).reserved() {
                reserved.entry(key).or_insert((side, disk));
            }
        }
        let mut found = Vec::new();
        for (key, (side, disk)) in reserved {
            if self.is_frozen(key) {
                continue;
            }
            for holder in [Side::Source, Side::Destination] {
                if self.side(holder).contains(key) {
                    let reason = format!(
                        "left out: its name differs only in upper and lower case from “{disk}” on the {}, a name Charon or the drive keeps for itself",
                        side.word()
                    );
                    found.push((holder, key.clone(), reason));
                }
            }
        }
        found
    }

    /// Items on the other side that side `to`'s drive can't hold: names it can't take, files
    /// too big for it, links where it has none, and what it refused in an earlier sync. Only
    /// the topmost such place is listed.
    fn unholdable(&self, to: Side) -> Vec<(String, String)> {
        let (from, here) = (self.side(to.opposite()), self.side(to));
        let drive = here.volume().map(Volume::describe).unwrap_or_else(|| format!("{}'s drive", to.word()));
        let mut found: Vec<(String, String)> = Vec::new();
        for (key, entry) in from.iter() {
            if self.is_frozen(key) || found.last().is_some_and(|(top, _)| Keys::is_within(key, top)) {
                continue;
            }
            let existing = here.get(key);
            let reason = if let Some(why) = here.refused(key) {
                Some(format!("an earlier sync found that {}", why.reason(&drive)))
            } else if existing.is_none() {
                let name: String = Keys::name(from.spelling(key)).nfc().collect();
                let volume = here.volume();
                volume.and_then(|volume| volume.cannot_hold(&name)).or_else(|| match entry.kind {
                    EntryKind::Link if !here.holds_links() => Some(format!("it's a symbolic link, and the {drive} can't hold links")),
                    EntryKind::File => volume.and_then(|volume| volume.too_big(entry.size)),
                    _ => None,
                })
            } else if entry.is_file() && existing.is_some_and(Entry::is_file) {
                here.volume().and_then(|volume| volume.too_big(entry.size))
            } else {
                None
            };
            if let Some(reason) = reason {
                found.push((key.clone(), format!("left out: {reason}")));
            }
        }
        found
    }

    fn is_frozen(&self, key: &str) -> bool {
        self.frozen.contains(key) || Keys::ancestors(key).any(|folder| self.frozen.contains(folder))
    }

    pub fn side(&self, side: Side) -> &'a Snapshot {
        match side {
            Side::Source => self.source,
            Side::Destination => self.destination,
        }
    }

    /// Whether `key` is in a place nothing may touch.
    pub fn is_off_limits(&self, key: &str) -> bool {
        self.is_frozen(key) || self.blocked.contains(key) || Keys::ancestors(key).any(|folder| self.blocked.contains(folder))
    }

    /// Puts `key` and everything inside it off-limits for the rest of the plan.
    pub fn block(&mut self, key: &str) {
        self.blocked.insert(key.to_string());
    }

    /// Whether anything inside `folder` on `side` differs from that side's record in the
    /// baseline (was added or edited there since the last sync).
    pub fn holds_changes(&self, side: Side, folder: &str, baseline: &Baseline) -> bool {
        let snapshot = self.side(side);
        let shifting = snapshot.shifts_times();
        snapshot.inside(folder).any(|(key, entry)| !Entry::still_on(Some(entry), baseline.side(key, side), shifting))
    }

    /// A folder is only removed when everything inside it is removed by this plan too.
    /// Otherwise the removal is dropped (whatever stays keeps it), and anything that wanted
    /// to take the folder's place is dropped with a note.
    pub fn finish(&self, plan: &mut Plan) {
        let deleted: HashSet<(Side, String)> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Delete { side, path, .. } => Some((*side, path.clone())),
                _ => None,
            })
            .collect();
        let kept: Vec<(Side, String)> = plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Delete { side, path, item: EntryKind::Dir, .. } => {
                    let snapshot = self.side(*side);
                    let all_go = !snapshot.is_occupied(path)
                        && snapshot.inside(path).all(|(inner, _)| deleted.contains(&(*side, inner.clone())));
                    (!all_go).then(|| (*side, path.clone()))
                }
                _ => None,
            })
            .collect();
        if kept.is_empty() {
            return;
        }
        let kept: HashSet<(Side, String)> = kept.into_iter().collect();
        let mut clashes = Vec::new();
        plan.actions.retain(|a| match a {
            Action::Delete { side, path, item: EntryKind::Dir, .. } => !kept.contains(&(*side, path.clone())),
            Action::Copy { to, path, .. } if kept.contains(&(*to, path.clone())) => {
                clashes.push((*to, path.clone()));
                false
            }
            _ => true,
        });
        for (side, path) in clashes {
            plan.note(
                NoteKind::Blocked,
                Some(side),
                self.side(side).spelling(&path).to_string(),
                "left alone: a folder with this name is still in the way",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{FsKind, Volume};

    fn snap(items: &[(&str, Entry)], kind: FsKind) -> Snapshot {
        let mut s: Snapshot = items.iter().map(|(k, e)| (k.to_string(), *e)).collect();
        s.facts_mut().volume = Some(Volume { kind, fs: "x".into(), mount: "/m".into(), name: "Stick".into(), network: false, case_sensitive: false, refuses_links: false });
        s
    }

    #[test]
    fn items_the_receiving_drive_cant_hold_are_left_alone() {
        let source = snap(
            &[
                ("a:b.txt", Entry::file(1, 1)),
                ("big.mov", Entry::file(5_000_000_000, 1)),
                ("shortcut", Entry::link(b"x", 1)),
                ("fine.txt", Entry::file(1, 1)),
            ],
            FsKind::Apfs,
        );
        let destination = snap(&[], FsKind::Fat);
        let mut plan = Plan::default();
        let ground = Ground::new(&source, &destination, &[Side::Destination], &mut plan);
        assert!(ground.is_off_limits("a:b.txt"));
        assert!(ground.is_off_limits("big.mov"));
        assert!(ground.is_off_limits("shortcut"));
        assert!(!ground.is_off_limits("fine.txt"));
        assert_eq!(plan.notes.len(), 3);
        // The source never receives anything in a one-way job: nothing to check there.
        let ground = Ground::new(&destination, &source, &[], &mut Plan::default());
        assert!(!ground.is_off_limits("a:b.txt"));
    }

    #[test]
    fn what_a_drive_refused_in_an_earlier_sync_is_left_out_with_everything_inside() {
        let source = snap(
            &[
                ("q3: plans", Entry::dir()),
                ("q3: plans/a.txt", Entry::file(1, 1)),
                ("big.iso", Entry::file(9, 1)),
                ("fine.txt", Entry::file(1, 1)),
            ],
            FsKind::Ext,
        );
        let mut destination = snap(&[("big.iso", Entry::file(5, 1))], FsKind::Other);
        destination.refuse("q3: plans", crate::scan::Refusal::Name);
        destination.refuse("big.iso", crate::scan::Refusal::TooBig);
        let mut plan = Plan::default();
        let ground = Ground::new(&source, &destination, &[Side::Destination], &mut plan);
        assert!(ground.is_off_limits("q3: plans/a.txt"));
        assert!(ground.is_off_limits("big.iso"), "even over an older version the drive holds");
        assert!(!ground.is_off_limits("fine.txt"));
        assert_eq!(plan.notes.len(), 2, "{:?}", plan.notes);
        assert!(plan.notes.iter().all(|n| n.kind == NoteKind::LeftOut && n.reason.starts_with("left out: an earlier sync found that")));
    }

    #[test]
    fn a_name_left_out_on_one_side_is_off_limits_on_both_under_one_note() {
        let source = snap(
            &[
                ("channel", Entry::file(1, 1)),
                ("music", Entry::dir()),
                ("music/a.mp3", Entry::file(1, 1)),
                ("fine.txt", Entry::file(1, 1)),
            ],
            FsKind::Ext,
        );
        let mut destination = snap(&[], FsKind::Ext);
        destination.leave_out("channel".into(), Some("channel"), "isn't a file or a folder (a socket, pipe or device)");
        destination.leave_out("music".into(), Some("music"), "is a link or junction; Charon leaves those alone on Windows");
        destination.leave_out("b\u{fffd}d".into(), None, "its name isn't valid Unicode, so Charon can't carry it");
        let mut plan = Plan::default();
        let ground = Ground::new(&source, &destination, &[Side::Source, Side::Destination], &mut plan);
        assert!(ground.is_off_limits("channel"));
        assert!(ground.is_off_limits("music/a.mp3"), "with everything inside");
        assert!(!ground.is_off_limits("fine.txt"));
        assert_eq!(plan.notes.len(), 3, "one note for each item left out: {:?}", plan.notes);
        assert!(plan.notes.iter().all(|n| n.kind == NoteKind::LeftOut && n.side == Some(Side::Destination)));
    }

    #[test]
    fn unreadable_places_are_off_limits_with_everything_inside() {
        let mut source = snap(&[("a", Entry::dir()), ("a/b.txt", Entry::file(1, 1))], FsKind::Ext);
        source.freeze("a", "permission denied");
        let destination = snap(&[], FsKind::Ext);
        let mut plan = Plan::default();
        let ground = Ground::new(&source, &destination, &[Side::Destination], &mut plan);
        assert!(ground.is_off_limits("a/b.txt"));
        assert_eq!(plan.notes[0].kind, NoteKind::Unreadable);
    }
}
