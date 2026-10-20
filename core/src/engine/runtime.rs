//! The live state of a job: everything that isn't saved to disk.

use crate::engine::review::StoredReview;
use crate::model::{ConflictItem, JobSetup, PerSide, Place, Progress, VolumeLabel};
use crate::plan::{Action, Plan, Side};
use crate::run::{Refused, Unread};
use crate::scan::{Entry, ExcludeFilter, Snapshot, Volume};
use crate::store::Absence;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct JobRuntime {
    /// Set while a sync runs; setting it to true stops the run.
    pub cancel: Option<Arc<AtomicBool>>,
    /// The person pressed Stop on the running sync (not Charon quitting).
    pub stopped_by_person: bool,
    /// Runs nobody looks at are refused until the person confirms a list (they stopped a
    /// sync, or a run was held back). Kept here too, in case the job's record couldn't be saved.
    pub must_ask: bool,
    /// A run nobody looks at is due (full automation), and why. It starts as soon as nothing
    /// stands in its way, so a trigger is never lost while another job uses the folders.
    pub wanted: Option<String>,
    /// The running job whose folders the wanted run waits for.
    pub waiting_for: Option<String>,
    /// Set while a review is being prepared; setting it to true stops that.
    pub preparing: Option<Arc<AtomicBool>>,
    /// Set while a background check runs; setting it to true stops it.
    pub checking: Option<Arc<AtomicBool>>,
    pub progress: Option<Progress>,
    /// What the last look found.
    pub look: Option<Look>,
    /// The list waiting for the person's OK.
    pub review: Option<Arc<StoredReview>>,
    /// What went wrong in the last look or run.
    pub problem: Option<String>,
    pub last_checked: Option<Instant>,
    pub finished_at: Option<Instant>,
    pub last_progress_sent: Option<Instant>,
    pub reminder: Reminder,
    /// Pairs of files compared byte for byte: key → both items as compared, and whether
    /// they were identical. Reused while neither file changes.
    pub same_contents: HashMap<String, (Entry, Entry, bool)>,
    /// The drive of the source / destination turned out not to hold symbolic links.
    pub no_links: (bool, bool),
    /// Items a drive refused in a sync (by the side they were to go to, and key), with that
    /// drive. Looks leave them out while the item and the drive stay the same.
    pub refused: HashMap<(Side, String), (Refused, Option<Volume>)>,
    /// Files a sync couldn't read (by the side they are on, and key). While one still can't
    /// be read, looks list it as a place they can't read, and remove nothing on the side it
    /// should be copied to.
    pub unread: HashMap<(Side, String), Unread>,
    /// The job's exclude filter, to tell which folder changes matter: the setup it was made
    /// for, and the filter (matching case the way the job's drives do, once a look has
    /// found out how they do).
    pub filter: Option<(JobSetup, ExcludeFilter)>,
    /// Items looks found missing on a drive that may show changes late, and since when.
    pub missing: Missing,
    /// When the look was made whose changes the last automatic run set out to carry, if
    /// that run ended before carrying anything (a folder it couldn't read, Charon's state
    /// that couldn't be saved). Changes found by that look aren't tried again until a newer
    /// look finds them: a run that fails every time mustn't start every second.
    pub tried: Option<DateTime<Utc>>,
}

impl JobRuntime {
    pub fn is_running(&self) -> bool {
        self.cancel.is_some()
    }

    pub fn is_checking(&self) -> bool {
        self.checking.is_some()
    }

    /// Stops a running check or review preparation (the job changed under them).
    pub fn stop_looking(&mut self) {
        for flag in [self.checking.as_ref(), self.preparing.as_ref()].into_iter().flatten() {
            flag.store(true, Ordering::Relaxed);
        }
    }

    /// Forgets everything learned about the folders (the job's folders or mode changed).
    pub fn forget_folders(&mut self) {
        self.stop_looking();
        self.look = None;
        self.review = None;
        self.problem = None;
        self.same_contents.clear();
        self.no_links = (false, false);
        self.refused.clear();
        self.unread.clear();
        self.filter = None;
        self.missing = Missing::default();
        self.tried = None;
        self.wanted = None;
        self.waiting_for = None;
    }
}

/// Items missing on a drive that may show changes late (a network share, a cloud drive),
/// and since when looks have found them missing. Such a drive can list a folder before it
/// has loaded, or from an old picture of it, so a run nobody looks at removes what follows
/// from an item's absence only once it has been missing for a while (`MISSING_FOR`). A list
/// the person confirms shows every removal and isn't held back.
///
/// When each item was first found missing is kept across launches (see `store::Absences`).
/// Right after Charon starts, a share may list a folder before it has loaded again, so an
/// item found missing before the launch also has to be found missing by the looks since
/// then for a while (`MISSING_AGAIN_FOR`).
#[derive(Debug, Default)]
pub(crate) struct Missing {
    noted: HashMap<(Side, String), Noted>,
}

/// One item of `Missing`.
#[derive(Debug, Clone, Copy)]
struct Noted {
    /// When a look first found it missing, by the clock on the wall.
    since: DateTime<Utc>,
    /// When a look since Charon started first found it missing.
    this_launch: Option<DateTime<Utc>>,
    /// A run nobody looked at left the removal that follows it for later.
    left_for_later: bool,
}

/// How long an item must have been missing before a run nobody looks at follows it.
pub(crate) const MISSING_FOR: Duration = Duration::from_secs(10 * 60);
/// How long the looks since Charon started must have found an item missing that was
/// already missing before.
pub(crate) const MISSING_AGAIN_FOR: Duration = Duration::from_secs(2 * 60);

impl Missing {
    /// The items kept from before Charon started.
    pub fn restored(kept: Vec<Absence>) -> Self {
        let noted = kept
            .into_iter()
            .map(|absence| ((absence.side, absence.key), Noted { since: absence.since, this_launch: None, left_for_later: absence.left_for_later }))
            .collect();
        Self { noted }
    }

    /// The items to keep for after Charon starts again, in a steady order.
    pub fn kept(&self) -> Vec<Absence> {
        let mut kept: Vec<Absence> = self
            .noted
            .iter()
            .map(|((side, key), noted)| Absence { side: *side, key: key.clone(), since: noted.since, left_for_later: noted.left_for_later })
            .collect();
        kept.sort_by(|a, b| (a.side, &a.key).cmp(&(b.side, &b.key)));
        kept
    }

    /// Notes the items whose absence `plan` follows with a removal on the other side, where
    /// they are missing on a drive that may show changes late. Every other item is
    /// forgotten: one that came back starts afresh if it goes missing again.
    pub fn note(&mut self, plan: &Plan, source: &Snapshot, destination: &Snapshot, now: DateTime<Utc>) {
        let found: HashSet<(Side, String)> = plan.actions.iter().filter_map(|action| Self::absence(action, source, destination)).collect();
        self.noted.retain(|item, _| found.contains(item));
        for item in found {
            let noted = self.noted.entry(item).or_insert(Noted { since: now, this_launch: None, left_for_later: false });
            noted.this_launch.get_or_insert(now);
        }
    }

    /// Takes out of `plan` the removals that follow an item missing on such a drive for less
    /// than `MISSING_FOR` (or not noted at all), and returns them. They are left for later:
    /// a later run carries them out if the item is still missing then (`take_follow_up`).
    pub fn hold_back_recent(&mut self, plan: &mut Plan, source: &Snapshot, destination: &Snapshot, now: DateTime<Utc>) -> Vec<Action> {
        for noted in self.noted.values_mut() {
            noted.left_for_later = false;
        }
        let (carried, held): (Vec<Action>, Vec<Action>) =
            std::mem::take(&mut plan.actions).into_iter().partition(|action| match Self::absence(action, source, destination) {
                Some(item) => match self.noted.get_mut(&item) {
                    Some(noted) if Self::lasted(noted, now) => true,
                    Some(noted) => {
                        noted.left_for_later = true;
                        false
                    }
                    None => false,
                },
                None => true,
            });
        plan.actions = carried;
        held
    }

    /// How many of the noted removals a run nobody looks at would still leave at `now`.
    pub fn waiting(&self, now: DateTime<Utc>) -> u64 {
        self.noted.values().filter(|noted| !Self::lasted(noted, now)).count() as u64
    }

    /// Whether a removal a run left for later can be carried out at `now`.
    pub fn follow_up_due(&self, now: DateTime<Utc>) -> bool {
        self.noted.values().any(|noted| noted.left_for_later && Self::lasted(noted, now))
    }

    /// `follow_up_due`, after which such removals are no longer marked: the run that
    /// carries them marks what it leaves again, and a run that ends before (a folder out of
    /// reach) doesn't start again every second.
    pub fn take_follow_up(&mut self, now: DateTime<Utc>) -> bool {
        let due = self.follow_up_due(now);
        if due {
            for noted in self.noted.values_mut() {
                noted.left_for_later = false;
            }
        }
        due
    }

    /// Whether an item has been missing long enough at `now` for a run nobody looks at. A
    /// clock set back never makes it so.
    fn lasted(noted: &Noted, now: DateTime<Utc>) -> bool {
        let since = |start: DateTime<Utc>, wait: Duration| (now - start).to_std().is_ok_and(|gone| gone >= wait);
        since(noted.since, MISSING_FOR) && noted.this_launch.is_some_and(|first| since(first, MISSING_AGAIN_FOR))
    }

    /// The item whose absence `action` follows, if it's a removal that does and the item is
    /// missing on a drive that may show changes late.
    fn absence(action: &Action, source: &Snapshot, destination: &Snapshot) -> Option<(Side, String)> {
        let Action::Delete { side, path, other: None, .. } = action else { return None };
        let missing_on = side.opposite();
        let snapshot = match missing_on {
            Side::Source => source,
            Side::Destination => destination,
        };
        snapshot.volume().is_some_and(Volume::shows_changes_late).then(|| (missing_on, path.clone()))
    }
}

/// What a look at both folders found (a check, a review, or a run's scan).
#[derive(Debug, Clone)]
pub(crate) struct Look {
    /// The job's setup the look was made with.
    pub setup: JobSetup,
    pub at: DateTime<Utc>,
    /// Steps that change something.
    pub items: u64,
    pub bytes: u64,
    pub removals: PerSide,
    pub replacements: PerSide,
    pub conflicts: Vec<ConflictItem>,
    /// At most `PLACES_SHOWN` of each.
    pub unreadable: Vec<Place>,
    pub not_synced: Vec<Place>,
    pub not_synced_count: u64,
    pub volumes: Option<(VolumeLabel, VolumeLabel)>,
}

impl Look {
    /// Whether the look found anything waiting for the person: changes or conflicts.
    pub fn has_work(&self) -> bool {
        self.items > 0 || !self.conflicts.is_empty()
    }
}

/// When the person was last told that changes wait for their OK.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Reminder {
    /// The last notification about this job (kept when the changes are confirmed, so a new
    /// wait right after doesn't notify again at once). By the clock on the wall: the time
    /// a computer sleeps counts too, so "daily" means daily.
    pub last: Option<DateTime<Utc>>,
    /// Notifications sent since changes began to wait.
    pub count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::FsKind;

    /// A folder holding `paths`, on a network share or on the computer's own drive.
    fn folder(paths: &[&str], share: bool) -> Snapshot {
        let mut snapshot: Snapshot = paths.iter().map(|path| (path.to_string(), Entry::file(1, 1))).collect();
        let kind = if share { FsKind::Smb } else { FsKind::Ext };
        snapshot.facts_mut().volume =
            Some(Volume { kind, fs: "x".into(), mount: "/x".into(), name: "Drive".into(), network: share, case_sensitive: true, case_guessed: false, refuses_links: false });
        snapshot
    }

    /// A Mirror's plan: the destination follows the source, which no longer has `path`.
    fn removing(path: &str) -> Plan {
        let mut plan = Plan::default();
        plan.push(Action::delete(Side::Destination, path, Entry::file(1, 1), None));
        plan
    }

    /// How many steps of `removing(path)` a run nobody looks at would carry out at `now`.
    fn carried(missing: &mut Missing, path: &str, source: &Snapshot, destination: &Snapshot, now: DateTime<Utc>) -> usize {
        let mut plan = removing(path);
        missing.hold_back_recent(&mut plan, source, destination, now);
        plan.actions.len()
    }

    fn minutes(n: i64) -> chrono::Duration {
        chrono::Duration::minutes(n)
    }

    #[test]
    fn a_removal_that_follows_a_share_waits_until_the_item_has_been_missing_a_while() {
        let (source, destination) = (folder(&[], true), folder(&["a.txt"], false));
        let mut missing = Missing::default();
        let start = Utc::now();
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start), 0, "never noted: it waits");
        missing.note(&removing("a.txt"), &source, &destination, start);
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start), 0);
        // Still missing at a later look: the removal goes ahead.
        let later = start + minutes(10);
        missing.note(&removing("a.txt"), &source, &destination, later);
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, later), 1);
    }

    #[test]
    fn an_item_that_came_back_starts_afresh() {
        let (source, destination) = (folder(&[], true), folder(&["a.txt"], false));
        let mut missing = Missing::default();
        let start = Utc::now();
        missing.note(&removing("a.txt"), &source, &destination, start);
        // A look in between finds it again (nothing to remove).
        missing.note(&Plan::default(), &source, &destination, start + minutes(5));
        let later = start + minutes(10);
        missing.note(&removing("a.txt"), &source, &destination, later);
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, later), 0);
    }

    #[test]
    fn removals_that_follow_the_computers_own_drive_or_make_room_never_wait() {
        let mut missing = Missing::default();
        let now = Utc::now();
        let (source, destination) = (folder(&[], false), folder(&["a.txt"], false));
        missing.note(&removing("a.txt"), &source, &destination, now);
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, now), 1);
        // A folder that became a file on the share: the removal makes room for the file.
        let (source, destination) = (folder(&["x"], true), folder(&["x/inner.txt"], false));
        let mut plan = Plan::default();
        plan.push(Action::delete(Side::Destination, "x", Entry::dir(), Some(Entry::file(1, 1))));
        missing.note(&plan, &source, &destination, now);
        assert!(missing.hold_back_recent(&mut plan, &source, &destination, now).is_empty());
    }

    #[test]
    fn an_item_missing_before_charon_started_is_looked_for_again_first() {
        let (source, destination) = (folder(&[], true), folder(&["a.txt"], false));
        let start = Utc::now();
        let mut before = Missing::default();
        before.note(&removing("a.txt"), &source, &destination, start - minutes(60 * 24));
        let mut missing = Missing::restored(before.kept());
        assert_eq!(missing.kept(), before.kept(), "kept as it was");
        // Missing for a day, but no look since the launch has found it missing yet.
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start), 0);
        missing.note(&removing("a.txt"), &source, &destination, start);
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start + minutes(1)), 0);
        missing.note(&removing("a.txt"), &source, &destination, start + minutes(2));
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start + minutes(2)), 1);
    }

    #[test]
    fn removals_left_for_later_are_followed_up_once_they_are_due() {
        let (source, destination) = (folder(&[], true), folder(&["a.txt"], false));
        let mut missing = Missing::default();
        let start = Utc::now();
        missing.note(&removing("a.txt"), &source, &destination, start);
        assert_eq!(missing.waiting(start), 1);
        assert!(!missing.take_follow_up(start + minutes(10)), "no run left anything for later");
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start), 0);
        assert!(missing.kept()[0].left_for_later);
        assert!(!missing.take_follow_up(start + minutes(9)));
        assert!(missing.take_follow_up(start + minutes(10)));
        assert_eq!(missing.waiting(start + minutes(10)), 0);
        assert!(!missing.take_follow_up(start + minutes(11)), "one follow-up, until a run leaves something for later again");
    }

    #[test]
    fn a_clock_set_back_never_makes_a_removal_due() {
        let (source, destination) = (folder(&[], true), folder(&["a.txt"], false));
        let mut missing = Missing::default();
        let start = Utc::now();
        missing.note(&removing("a.txt"), &source, &destination, start);
        assert_eq!(carried(&mut missing, "a.txt", &source, &destination, start - minutes(60)), 0);
    }
}
