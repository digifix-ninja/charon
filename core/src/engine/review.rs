//! Reviews: the list of changes a sync would make, waiting for the person's OK.
//!
//! A review is made from a fresh survey and kept (one per job) until it's confirmed,
//! discarded, replaced by a newer one, or the job changes. Confirming it carries out exactly
//! that list: nothing else, and each step only if its items are still as they were seen. A
//! list older than 15 minutes, or made before something changed in the folders, can't be
//! confirmed: the person looks again.

use crate::engine::runs::Crossing;
use crate::engine::survey::{Purpose, Survey};
use crate::engine::{Engine, Look};
use crate::error::{Error, Result};
use crate::model::{
    Cluster, ConflictChoice, ConflictItem, JobId, Level, ReviewKind, Review, ReviewNote, ReviewStep, Shore, StepFilter, StepKind,
    Tally, VolumeLabel, Warning,
};
use crate::plan::{Action, NoteKind, Resolution, Side, Unit};
use crate::run::Markers;
use crate::scan::{Entry, EntryKind, Keys, RootId};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// How long a list can be confirmed after it was made.
const VALID_FOR_MINUTES: i64 = 15;
/// Steps and notes in the first page of a review.
const FIRST_STEPS: usize = 200;
const FIRST_NOTES: usize = 100;
/// The most steps or notes one page can hold.
const MAX_PAGE: usize = 500;
/// Removed folders listed on their own.
const CLUSTERS: usize = 50;

/// A review as the engine keeps it.
pub(crate) struct StoredReview {
    pub id: String,
    pub kind: ReviewKind,
    pub title: Option<String>,
    pub survey: Survey,
    pub prepared_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Something changed in the folders after the list was made.
    pub stale: AtomicBool,
    /// When the job's last run had finished when the list was made.
    pub runs_seen: Option<DateTime<Utc>>,
    pub steps: Vec<ReviewStep>,
    pub notes: Vec<ReviewNote>,
    pub warnings: Vec<Warning>,
    pub blocked: Option<String>,
    pub needs_tick: bool,
    /// The conflict a resolution settles, by its key.
    pub resolving: Option<String>,
}

/// One page of a review's steps.
#[derive(Debug, Clone, Serialize)]
pub struct StepPage {
    pub steps: Vec<ReviewStep>,
    pub total: usize,
}

/// One page of a review's notes.
#[derive(Debug, Clone, Serialize)]
pub struct NotePage {
    pub notes: Vec<ReviewNote>,
    pub total: usize,
}

impl Engine {
    /// Looks at both folders now and prepares the list of what the next sync would do.
    /// Slow: it scans both folders (`cancel_review` stops it).
    pub fn prepare_review(&self, id: &JobId) -> Result<Review> {
        let (job, stop) = self.begin_look(id)?;
        let survey = self.survey(&job, &stop, Purpose::Review);
        self.end_look(id, &stop);
        let survey = survey?;
        self.note_look(&job, &survey);
        let review = self.keep_review(&job, survey, ReviewKind::Sync, None, None)?;
        self.publish(id);
        Ok(review)
    }

    /// Stops preparing a review of `id`.
    pub fn cancel_review(&self, id: &JobId) {
        if let Some(stop) = self.runtime().get(id).and_then(|rt| rt.preparing.clone()) {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// The list the person closed without confirming it.
    pub fn discard_review(&self, id: &JobId, review_id: &str) {
        self.change_runtime(id, |rt| {
            if rt.review.as_ref().is_some_and(|review| review.id == review_id) {
                rt.review = None;
            }
        });
    }

    /// One page of a review's steps, filtered by kind and by a part of the path.
    pub fn review_steps(&self, id: &JobId, review_id: &str, filter: StepFilter, query: &str, offset: usize, limit: usize) -> Result<StepPage> {
        self.review_steps_in(id, review_id, filter, query, None, offset, limit)
    }

    /// One page of a review's steps, like `review_steps`, and only those at or inside
    /// `folder` (a path as the list spells it) when one is given: what goes with a removed
    /// folder, and nothing from another folder whose path merely contains its name.
    #[allow(clippy::too_many_arguments)]
    pub fn review_steps_in(
        &self,
        id: &JobId,
        review_id: &str,
        filter: StepFilter,
        query: &str,
        folder: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<StepPage> {
        let review = self.stored_review(id, review_id)?;
        let query = query.trim().to_lowercase();
        let folder = folder.map(|folder| folder.trim_end_matches('/')).filter(|folder| !folder.is_empty());
        let inside = |path: &str| {
            folder.is_none_or(|folder| path.strip_prefix(folder).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')))
        };
        let matching: Vec<&ReviewStep> = review
            .steps
            .iter()
            .filter(|step| filter.admits(step) && inside(&step.path) && (query.is_empty() || step.path.to_lowercase().contains(&query)))
            .collect();
        let steps = matching.iter().skip(offset).take(limit.min(MAX_PAGE)).map(|step| (*step).clone()).collect();
        Ok(StepPage { steps, total: matching.len() })
    }

    /// One page of a review's notes.
    pub fn review_notes(&self, id: &JobId, review_id: &str, offset: usize, limit: usize) -> Result<NotePage> {
        let review = self.stored_review(id, review_id)?;
        let notes = review.notes.iter().skip(offset).take(limit.min(MAX_PAGE)).cloned().collect();
        Ok(NotePage { notes, total: review.notes.len() })
    }

    /// Carries out exactly the reviewed list, in the background. Refused when the list
    /// can't be trusted anymore, or can't be confirmed as it is.
    pub fn approve(&self, id: &JobId, review_id: &str, acknowledged: bool) -> Result<()> {
        let review = self.stored_review(id, review_id)?;
        // The folders must still be the very folders the list was made from.
        for side in [Side::Source, Side::Destination] {
            let root = review.survey.setup.root(side);
            if !root.is_dir() {
                return Err(Error::Offline(root.to_path_buf()));
            }
            let now = RootId::of(root).map_err(|e| Error::file(root, e))?;
            if let (Some(now), Some(then)) = (now, review.survey.snapshot(side).root()) {
                if !now.same_folder(&then) {
                    return Err(Error::ReviewOutdated(format!(
                        "The {} folder isn't the one this list was made for anymore (was a drive changed?). Look again.",
                        side.word()
                    )));
                }
            }
        }
        // And carry the same markers: a drive swapped for another one under the same name
        // can keep the folder's numbers, but not Charon's marker.
        let markers = Markers::read(review.survey.setup.root(Side::Source), review.survey.setup.root(Side::Destination), id);
        if let Some(side) = markers.changed_since(&review.survey.markers) {
            return Err(Error::ReviewOutdated(format!(
                "Charon's marker in the {} folder changed since this list was made (was a drive changed?). Look again.",
                side.word()
            )));
        }
        let (job, cancel) = self.begin_run(id, |job, rt| {
            let current = rt.review.as_ref().filter(|r| r.id == review_id).ok_or_else(Self::gone)?;
            if Utc::now() > current.expires_at {
                return Err(Error::ReviewOutdated(format!("This list was made more than {VALID_FOR_MINUTES} minutes ago. Look again.")));
            }
            if current.stale.load(Ordering::Relaxed) {
                return Err(Error::ReviewOutdated("Things changed in the folders since this list was made. Look again.".into()));
            }
            if job.setup != current.survey.setup {
                return Err(Error::ReviewOutdated("The job's settings changed since this list was made. Look again.".into()));
            }
            if job.last_run.as_ref().map(|run| run.finished_at) != current.runs_seen {
                return Err(Error::ReviewOutdated("The job synced since this list was made. Look again.".into()));
            }
            if let Some(blocked) = &current.blocked {
                return Err(Error::Invalid(blocked.clone()));
            }
            if current.needs_tick && !acknowledged {
                return Err(Error::Invalid("Tick “I've looked at the removals and replacements” first.".into()));
            }
            rt.review = None;
            Ok(())
        })?;
        self.publish(id);
        let reason = match review.kind {
            ReviewKind::Sync => "confirmed by you".to_string(),
            ReviewKind::Resolve => format!("to settle a conflict ({})", review.title.as_deref().unwrap_or("confirmed by you")),
        };
        let crossing = Crossing { kind: review.kind, approved: true, reason, resolving: review.resolving.clone() };
        self.spawn_crossing(job, cancel, crossing, move |engine, job, cancel, crossing| {
            engine.cross(&job, &review.survey, &cancel, &crossing);
        })
    }

    /// Prepares the list that settles one conflict the way the person chose. Nothing
    /// changes until it's confirmed like any other list.
    pub fn prepare_resolution(&self, id: &JobId, conflict: &ConflictItem, choice: ConflictChoice) -> Result<Review> {
        let (job, stop) = self.begin_look(id)?;
        if !job.setup.mode.is_two_way() {
            self.end_look(id, &stop);
            return Err(Error::Invalid("Only two-way jobs have conflicts to settle.".into()));
        }
        let survey = self.survey(&job, &stop, Purpose::Review);
        self.end_look(id, &stop);
        let mut survey = survey?;
        let key = conflict.key.as_str();
        let now = Self::conflict_item(&survey, key, survey.source.get(key).copied(), survey.destination.get(key).copied());
        // Still a conflict now, and the same one (for an app library or a database, a
        // conflict means its files still changed on both sides).
        let still_open = survey.plan.conflicts().iter().any(|(path, ..)| *path == key);
        if &now != conflict || !still_open {
            self.note_look(&job, &survey);
            self.publish(id);
            return Err(Error::ReviewOutdated("It changed since you saw it. Look again.".into()));
        }
        let plan = Resolution::plan(&survey.source, &survey.destination, key, choice).map_err(Error::Invalid)?;
        let title = format!(
            "Settle “{}”: {}",
            conflict.path,
            match choice {
                ConflictChoice::KeepSource => "keep the source's version",
                ConflictChoice::KeepDestination => "keep the destination's version",
                ConflictChoice::KeepBoth => "keep both versions",
            }
        );
        survey.plan = plan;
        survey.refusal = None;
        survey.holds.clear();
        survey.needs_tick = false;
        survey.agreed = 0;
        survey.housekeeping = Default::default();
        // What's said about the drives themselves still holds (another drive, a missing marker).
        survey.warnings.retain(|warning| warning.level == Level::Danger);
        if !survey.trusted && survey.memory.as_ref().is_some_and(|memory| memory.tokens.is_some()) {
            survey.warnings.push(Warning::caution(
                "Charon doesn't trust its memory of the last sync right now, so settling this item lets go of that memory. \
                 Until you confirm a whole list of this job's changes, Charon compares both folders afresh and removes nothing.",
            ));
        }
        survey.warnings.push(Warning::info(
            "Only this item changes. The version that is replaced or removed goes to .charon-trash, even when safe deletes are off.",
        ));
        let review = self.keep_review(&job, survey, ReviewKind::Resolve, Some(title), Some(key.to_string()))?;
        self.publish(id);
        Ok(review)
    }

    // ---------------------------------------------------------------- internals

    /// Marks a look (a review being prepared) as started, if the job isn't busy.
    fn begin_look(&self, id: &JobId) -> Result<(crate::model::Job, Arc<AtomicBool>)> {
        let neighbours = self.neighbours(id);
        let state = self.state();
        let job = state.jobs.iter().find(|job| &job.id == id).cloned().ok_or_else(|| Error::UnknownJob(id.clone()))?;
        let mut runtime = self.runtime();
        if self.is_shutting_down() {
            return Err(Error::Invalid("Charon is quitting.".into()));
        }
        if runtime.get(id).is_some_and(|rt| rt.is_running()) {
            return Err(Error::Busy);
        }
        if let Some(other) = Self::busy_neighbour(&state.jobs, &runtime, &job, &neighbours) {
            return Err(Error::FoldersBusy(other));
        }
        let rt = runtime.entry(id.clone()).or_default();
        if rt.preparing.is_some() {
            return Err(Error::Invalid("Charon is already looking at this job's folders.".into()));
        }
        // A background look at the same folders would only read them twice.
        if let Some(checking) = &rt.checking {
            checking.store(true, Ordering::Relaxed);
        }
        let stop = Arc::new(AtomicBool::new(false));
        rt.preparing = Some(stop.clone());
        Ok((job, stop))
    }

    fn end_look(&self, id: &JobId, stop: &Arc<AtomicBool>) {
        self.change_runtime(id, |rt| {
            if rt.preparing.as_ref().is_some_and(|flag| Arc::ptr_eq(flag, stop)) {
                rt.preparing = None;
            }
        });
    }

    /// A conflict as the window shows it, saying when it's about a whole app library or
    /// database.
    fn conflict_item(survey: &Survey, key: &str, source: Option<Entry>, destination: Option<Entry>) -> ConflictItem {
        let unit = Unit::of(key, |k| survey.source.contains(k) || survey.destination.contains(k))
            .filter(|unit| matches!(unit, Unit::Database(_)) || unit.key() == key);
        ConflictItem::new(key, &survey.spelling(key), source, destination).whole(unit.as_ref())
    }

    fn gone() -> Error {
        Error::ReviewOutdated("This list is no longer current. Look again.".into())
    }

    fn stored_review(&self, id: &JobId, review_id: &str) -> Result<Arc<StoredReview>> {
        self.runtime().get(id).and_then(|rt| rt.review.clone()).filter(|review| review.id == review_id).ok_or_else(Self::gone)
    }

    /// Keeps a review of `survey` for the job (replacing any other) and returns it.
    fn keep_review(&self, job: &crate::model::Job, survey: Survey, kind: ReviewKind, title: Option<String>, resolving: Option<String>) -> Result<Review> {
        let prepared_at = Utc::now();
        let steps: Vec<ReviewStep> = survey.plan.for_review().into_iter().map(|action| Self::step_of(&survey, action, kind)).collect();
        let notes: Vec<ReviewNote> = survey
            .plan
            .notes
            .iter()
            .map(|note| ReviewNote {
                kind: note.kind,
                side: note.side,
                path: note.path.clone(),
                reason: note.reason.clone(),
                can_leave_out: note.kind == NoteKind::Unreadable && note.side.is_some(),
            })
            .collect();
        let stored = Arc::new(StoredReview {
            id: uuid::Uuid::new_v4().simple().to_string(),
            kind,
            title,
            prepared_at,
            expires_at: prepared_at + Duration::minutes(VALID_FOR_MINUTES),
            stale: AtomicBool::new(false),
            runs_seen: job.last_run.as_ref().map(|run| run.finished_at),
            warnings: survey.warnings.clone(),
            blocked: survey.refusal.clone(),
            needs_tick: survey.needs_tick,
            resolving,
            steps,
            notes,
            survey,
        });
        let review = self.review_of(job, &stored);
        self.change_runtime(&job.id, |rt| {
            rt.review = Some(stored);
            // The person is looking at the list: the next reminder can wait.
            rt.reminder.last = Some(Utc::now());
            rt.reminder.count = rt.reminder.count.max(1);
        });
        Ok(review)
    }

    /// The review as the window receives it.
    fn review_of(&self, job: &crate::model::Job, stored: &StoredReview) -> Review {
        let survey = &stored.survey;
        let shore = |side: Side| Shore {
            path: survey.setup.root(side).display().to_string(),
            volume: survey.volume(side).map(VolumeLabel::of),
            files: survey.snapshot(side).file_count() as u64,
        };
        let mut housekeeping = survey.housekeeping.lines(self.settings().trash_days);
        housekeeping.retain(|line| !line.is_empty());
        Review {
            review_id: stored.id.clone(),
            job_id: job.id.clone(),
            job_name: job.setup.name.clone(),
            kind: stored.kind,
            title: stored.title.clone(),
            mode: survey.setup.mode,
            safe_delete: survey.setup.safe_delete,
            full_automation: survey.setup.full_automation,
            prepared_at: stored.prepared_at,
            expires_at: stored.expires_at,
            source: shore(Side::Source),
            destination: shore(Side::Destination),
            tally: Self::tally_of(survey),
            warnings: stored.warnings.clone(),
            blocked: stored.blocked.clone(),
            needs_tick: stored.needs_tick,
            clusters: Self::clusters_of(survey),
            housekeeping,
            total_steps: stored.steps.len(),
            total_notes: stored.notes.len(),
            steps: stored.steps.iter().take(FIRST_STEPS).cloned().collect(),
            notes: stored.notes.iter().take(FIRST_NOTES).cloned().collect(),
        }
    }

    fn tally_of(survey: &Survey) -> Tally {
        let mut tally = Tally { agreed: survey.agreed, ..Tally::default() };
        for action in &survey.plan.actions {
            match action {
                Action::CreateDir { side, .. } => tally.side_mut(*side).new_folders += 1,
                Action::Copy { to, bytes, replaces, settles_conflict, .. } => {
                    let side = tally.side_mut(*to);
                    if *replaces {
                        side.replaced += 1;
                    } else {
                        side.new_files += 1;
                    }
                    side.bytes_in += bytes;
                    tally.bytes += bytes;
                    if *settles_conflict {
                        tally.newest_wins += 1;
                    }
                }
                Action::Delete { side, item, .. } => {
                    let side = tally.side_mut(*side);
                    if *item == EntryKind::Dir {
                        side.removed_folders += 1;
                    } else {
                        side.removed += 1;
                    }
                }
                Action::KeepBoth { source_bytes, destination_bytes, .. } => {
                    tally.keep_both += 1;
                    tally.destination.bytes_in += source_bytes;
                    tally.source.bytes_in += destination_bytes;
                    tally.bytes += source_bytes + destination_bytes;
                }
                Action::Conflict { .. } => tally.conflicts += 1,
            }
        }
        tally
    }

    /// Whole folders that go, with what's inside them, biggest first.
    fn clusters_of(survey: &Survey) -> Vec<Cluster> {
        let mut tops: BTreeMap<(Side, String), (u64, u64)> = BTreeMap::new();
        let removed_folders: Vec<(Side, &str)> = survey
            .plan
            .actions
            .iter()
            .filter_map(|a| match a {
                Action::Delete { side, path, item: EntryKind::Dir, .. } => Some((*side, path.as_str())),
                _ => None,
            })
            .collect();
        for &(side, path) in &removed_folders {
            let nested = Keys::ancestors(path).any(|folder| removed_folders.contains(&(side, folder)));
            if !nested {
                tops.insert((side, path.to_string()), (0, 0));
            }
        }
        for action in &survey.plan.actions {
            if let Action::Delete { side, path, entry, .. } = action {
                let top = Keys::ancestors(path).find(|folder| tops.contains_key(&(*side, folder.to_string())));
                if let Some(top) = top {
                    if let Some((items, bytes)) = tops.get_mut(&(*side, top.to_string())) {
                        *items += 1;
                        if entry.is_file() {
                            *bytes += entry.size;
                        }
                    }
                }
            }
        }
        let mut clusters: Vec<Cluster> = tops
            .into_iter()
            .map(|((side, key), (items, bytes))| Cluster { side, path: survey.snapshot(side).spelling(&key).to_string(), items, bytes })
            .collect();
        clusters.sort_by(|a, b| b.items.cmp(&a.items).then_with(|| b.bytes.cmp(&a.bytes)).then_with(|| a.path.cmp(&b.path)));
        clusters.truncate(CLUSTERS);
        clusters
    }

    /// One step as the review shows it, with why it happens.
    fn step_of(survey: &Survey, action: &Action, kind: ReviewKind) -> ReviewStep {
        let two_way = survey.setup.mode.is_two_way();
        let resolving = kind == ReviewKind::Resolve;
        let kept = survey.setup.safe_delete || resolving;
        let path = survey.spelling(action.path());
        let chosen = |side: Side| format!("you chose the {}'s version", side.opposite().word());
        match action {
            Action::CreateDir { side, .. } => ReviewStep {
                kind: StepKind::Folder,
                side: Some(*side),
                path,
                item: EntryKind::Dir,
                bytes: 0,
                reason: if resolving {
                    chosen(*side)
                } else if two_way {
                    format!("new folder on the {}", side.opposite().word())
                } else {
                    "a folder in the source".into()
                },
                danger: false,
            },
            Action::Copy { to, item, bytes, replaces, settles_conflict, from, existing, .. } => {
                let fate = if kept || two_way { "the replaced version goes to .charon-trash" } else { "the replaced version is gone for good" };
                let (step, reason) = if resolving {
                    if *replaces {
                        (StepKind::Replace, format!("{}; this side's goes to .charon-trash", chosen(*to)))
                    } else {
                        (StepKind::New, chosen(*to))
                    }
                } else if *settles_conflict {
                    (
                        StepKind::Settle,
                        format!(
                            "changed on both sides; the {}'s version was saved last and replaces this one, which goes to .charon-trash",
                            to.opposite().word()
                        ),
                    )
                } else if *replaces {
                    let why = match existing {
                        Some(existing) if existing.kind != from.kind => format!("the {} has {} here now", Self::other_place(*to, two_way), Self::a(from.kind)),
                        _ if two_way => format!("changed on the {} since the last sync", to.opposite().word()),
                        _ => "changed in the source".into(),
                    };
                    (StepKind::Replace, format!("{why}; {fate}"))
                } else if two_way {
                    (StepKind::New, format!("new on the {}", to.opposite().word()))
                } else {
                    (StepKind::New, "in the source, not yet in the destination".into())
                };
                ReviewStep { kind: step, side: Some(*to), path, item: *item, bytes: *bytes, reason, danger: *replaces }
            }
            Action::Delete { side, item, other, .. } => {
                let fate = if kept { "goes to .charon-trash" } else { "gone for good" };
                let why = if resolving {
                    chosen(*side)
                } else if let Some(other) = other {
                    format!("makes room: the {} has {} here", Self::other_place(*side, two_way), Self::a(other.kind))
                } else if two_way {
                    format!("removed on the {} since the last sync", side.opposite().word())
                } else {
                    "no longer in the source".into()
                };
                let reason = if *item == EntryKind::Dir {
                    format!("{why}; removed once nothing else is left in it")
                } else {
                    format!("{why}; {fate}")
                };
                ReviewStep { kind: StepKind::Remove, side: Some(*side), path, item: *item, bytes: 0, reason, danger: *item != EntryKind::Dir }
            }
            Action::KeepBoth { source_bytes, destination_bytes, .. } => ReviewStep {
                kind: StepKind::KeepBoth,
                side: None,
                path,
                item: EntryKind::File,
                bytes: source_bytes + destination_bytes,
                reason: if resolving {
                    "you chose to keep both versions; the destination's gets a “(conflict …)” name on both sides".into()
                } else {
                    "changed on both sides: both versions are kept, the destination's under a “(conflict …)” name".into()
                },
                danger: false,
            },
            Action::Conflict { source, destination, path: key } => {
                let unit = Unit::of(key, |k| survey.source.contains(k) || survey.destination.contains(k))
                    .filter(|unit| matches!(unit, Unit::Database(_)) || unit.key() == key);
                let reason = match (source, destination) {
                    _ if unit.is_some() => format!(
                        "{} whose files changed on both sides: left for you to decide which side's copy to keep as a whole",
                        unit.as_ref().map_or("", Unit::describe)
                    ),
                    (Some(s), Some(d)) if s.kind != d.kind => {
                        format!("{} in the source, {} in the destination: left for you to decide", Self::a(s.kind), Self::a(d.kind))
                    }
                    _ => "changed on both sides: left for you to decide".into(),
                };
                ReviewStep {
                    kind: StepKind::Conflict,
                    side: None,
                    path,
                    item: source.or(*destination).map(|e| e.kind).unwrap_or(EntryKind::File),
                    bytes: 0,
                    reason,
                    danger: false,
                }
            }
        }
    }

    /// "the source" / "the destination" for the side across from `side`.
    fn other_place(side: Side, _two_way: bool) -> &'static str {
        side.opposite().word()
    }

    /// "a file", "a folder", "a link".
    fn a(kind: EntryKind) -> &'static str {
        match kind {
            EntryKind::File => "a file",
            EntryKind::Dir => "a folder",
            EntryKind::Link => "a link",
        }
    }

    /// Marks the job's review as outdated (something changed in its folders) and tells the window.
    pub(crate) fn mark_review_stale(&self, id: &JobId) {
        let stale = self.change_runtime(id, |rt| {
            rt.review.as_ref().filter(|review| !review.stale.swap(true, Ordering::Relaxed)).map(|review| review.id.clone())
        });
        if let Some(review_id) = stale {
            self.emit(crate::engine::EngineEvent::ReviewStale { id: id.clone(), review_id });
        }
    }

    /// Records what a survey found as the job's latest look, and since when its changes
    /// have waited.
    pub(crate) fn note_look(&self, job: &crate::model::Job, survey: &Survey) {
        let look = Look::of(survey);
        let at = look.at;
        let has_work = look.has_work();
        let held = self.stored_job(&job.id).ok().and_then(|stored| stored.held).is_some();
        self.change_runtime(&job.id, |rt| {
            if !has_work && !held {
                rt.reminder.count = 0;
            }
            rt.problem = None;
            rt.look = Some(look);
        });
        // Saved only when the wait begins or ends.
        let since = self.stored_job(&job.id).ok().and_then(|stored| stored.waiting_since);
        let wanted = if has_work || held { since.or(Some(at)) } else { None };
        if wanted != since {
            if let Err(e) = self.change_job(&job.id, |stored| stored.waiting_since = wanted) {
                log::warn!("couldn't save since when changes of {} wait: {e}", job.setup.name);
            }
        }
    }
}

impl Look {
    pub(crate) fn of(survey: &Survey) -> Look {
        let plan = &survey.plan;
        let summary = plan.summary();
        let mut removals = crate::model::PerSide::default();
        let mut replacements = crate::model::PerSide::default();
        for side in [Side::Source, Side::Destination] {
            removals.add(side, plan.removed_on(side));
            replacements.add(side, plan.destructive_on(side) - plan.removed_on(side));
        }
        let conflicts = plan
            .conflicts()
            .into_iter()
            .map(|(key, source, destination)| Engine::conflict_item(survey, key, source, destination))
            .collect();
        let places = |kinds: &[NoteKind]| -> Vec<crate::model::Place> {
            plan.notes
                .iter()
                .filter(|note| kinds.contains(&note.kind))
                .filter_map(|note| {
                    Some(crate::model::Place {
                        kind: note.kind,
                        side: note.side?,
                        path: note.path.clone(),
                        reason: note.reason.clone(),
                        privacy: note.reason == crate::scan::PRIVACY_REFUSAL,
                    })
                })
                .collect()
        };
        let mut unreadable = places(&[NoteKind::Unreadable]);
        unreadable.truncate(crate::model::PLACES_SHOWN);
        let mut not_synced = places(&[NoteKind::LeftOut, NoteKind::NameClash, NoteKind::Blocked]);
        let not_synced_count = not_synced.len() as u64;
        not_synced.truncate(crate::model::PLACES_SHOWN);
        let volumes = match (survey.volume(Side::Source), survey.volume(Side::Destination)) {
            (Some(s), Some(d)) => Some((VolumeLabel::of(s), VolumeLabel::of(d))),
            _ => None,
        };
        Look {
            setup: survey.setup.clone(),
            at: survey.made_at,
            items: summary.changes(),
            bytes: summary.copy_bytes,
            removals,
            replacements,
            conflicts,
            unreadable,
            not_synced,
            not_synced_count,
            volumes,
        }
    }
}
