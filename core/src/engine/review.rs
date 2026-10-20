//! Reviews: the list of changes a sync would make, waiting for the person's OK.
//!
//! A review is made from a fresh survey and kept (one per job) until it's confirmed,
//! discarded, replaced by a newer one, or the job changes. Confirming it carries out exactly
//! that list: nothing else, and each step only if its items are still as they were seen. A
//! list older than 15 minutes, or made before something changed in the folders, can't be
//! confirmed: the person looks again. A list confirmed while another sync is at work in the
//! job's folders waits for that sync, and is carried out as soon as it's done (unless the
//! job changed, synced, or a drive was swapped meanwhile: then the row says why it wasn't).

use crate::engine::runs::Crossing;
use crate::engine::runtime::{Confirmed, JobRuntime};
use crate::engine::survey::{Purpose, Survey};
use crate::engine::{Engine, Look};
use crate::error::{Error, Result};
use crate::model::{
    Cluster, ConflictChoice, ConflictItem, JobId, Level, ReviewKind, Review, ReviewNote, ReviewStep, Shore, StepFilter, StepKind,
    Tally, VolumeLabel, Warning,
};
use crate::plan::{Action, Clearing, NoteKind, Resolution, Side, Unit};
use crate::run::Markers;
use crate::scan::{Entry, EntryKind, Keys, RootId};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

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
    /// Why a list without steps is still worth confirming (see `Review::confirm_anyway`).
    pub confirm_anyway: Option<String>,
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

/// What confirming a list did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Approval {
    /// The sync started.
    Started,
    /// Another sync is at work in the job's folders: the confirmed sync starts by itself as
    /// soon as that sync is done, unless the person calls it off (Stop, or closing the list).
    Waiting {
        /// That sync, as the row's `waitingFor` names it: another job's name, "another
        /// Charon", or "Charon on <computer>".
        #[serde(rename = "waitingFor")]
        waiting_for: String,
    },
}

impl Engine {
    /// Looks at both folders now and prepares the list of what the next sync would do.
    /// Slow: it scans both folders (`cancel_review` stops it).
    pub fn prepare_review(&self, id: &JobId) -> Result<Review> {
        let (job, stop, epoch) = self.begin_look(id)?;
        let survey = self.survey(&job, &stop, Purpose::Review);
        self.end_look(id, &stop);
        let survey = survey?;
        if !self.note_look(&job, &survey, Some(epoch)) {
            return Err(Self::overtaken());
        }
        let review = self.keep_review(&job, survey, ReviewKind::Sync, None, None, epoch)?;
        self.publish(id);
        Ok(review)
    }

    /// Stops preparing a review of `id`.
    pub fn cancel_review(&self, id: &JobId) {
        if let Some(stop) = self.runtime().get(id).and_then(|rt| rt.preparing.clone()) {
            stop.store(true, Ordering::Relaxed);
        }
    }

    /// The list the person closed without confirming it (or, once confirmed, while its sync
    /// waited for another sync).
    pub fn discard_review(&self, id: &JobId, review_id: &str) {
        let waited = self.change_known_runtime(id, |rt| {
            let current = rt.review.as_ref().is_some_and(|review| review.id == review_id);
            let waited = current && rt.confirmed.is_some();
            if current {
                rt.forget_review();
            }
            waited
        });
        if waited == Some(true) {
            self.note_called_off(id, "");
            self.publish(id);
        }
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
    /// can't be trusted anymore, or can't be confirmed as it is. While another sync is at
    /// work in the job's folders, the confirmed list waits for it, and its sync starts as
    /// soon as that sync is done (see `start_confirmed`; the row says what it waits for).
    /// Says which of the two happened.
    pub fn approve(&self, id: &JobId, review_id: &str, acknowledged: bool) -> Result<Approval> {
        match self.carry_out(id, review_id, acknowledged, false) {
            Ok(()) => Ok(Approval::Started),
            Err(Error::FoldersBusy(who)) if self.queue_confirmed(id, review_id, acknowledged, &who) => Ok(Approval::Waiting { waiting_for: who }),
            // The list was replaced or closed in between.
            Err(Error::FoldersBusy(_)) => Err(Self::gone()),
            Err(e) => Err(e),
        }
    }

    /// Starts the sync of a list the person confirmed while another sync was at work in the
    /// job's folders, if nothing stands in its way anymore. Returns whether it started. A
    /// list that can't be carried out anymore (the job changed, synced, or a drive was
    /// swapped meanwhile) is dropped, and the row says why.
    pub fn start_confirmed(&self, id: &JobId) -> bool {
        if self.is_shutting_down() {
            return false;
        }
        let Some(confirmed) = self.runtime().get(id).and_then(|rt| rt.confirmed.clone()) else { return false };
        match self.carry_out(id, &confirmed.review_id, confirmed.acknowledged, true) {
            Ok(()) => true,
            Err(Error::FoldersBusy(who)) => {
                // Only while it's still the job's confirmed list: it may have been called off
                // meanwhile.
                let current = |rt: &JobRuntime| rt.confirmed.as_ref().is_some_and(|waiting| waiting.review_id == confirmed.review_id);
                let changed = self.change_known_runtime(id, |rt| current(rt) && rt.waiting_for.replace(who.clone()) != Some(who));
                if changed == Some(true) {
                    self.publish(id);
                }
                false
            }
            // Its own sync runs, or a folder is away for a moment: it keeps waiting. (Stopped
            // by the person meanwhile, it was called off and said so.)
            Err(Error::Busy) | Err(Error::Offline(_)) | Err(Error::Cancelled) => false,
            Err(e) => {
                self.change_known_runtime(id, |rt| {
                    if rt.confirmed.as_ref().is_some_and(|waiting| waiting.review_id == confirmed.review_id) {
                        rt.forget_review();
                        rt.problem = Some(format!("The list you confirmed couldn't be carried out after all. {e}"));
                    }
                });
                self.inner.journal.warn(id, &format!("the confirmed sync that waited for another sync didn't start: {e}"));
                self.publish(id);
                false
            }
        }
    }

    /// Keeps a confirmed list waiting while another sync (`who`) is at work in the job's
    /// folders. Returns whether it waits (it's still the job's list).
    fn queue_confirmed(&self, id: &JobId, review_id: &str, acknowledged: bool, who: &str) -> bool {
        let queued = self.change_known_runtime(id, |rt| {
            let current = rt.review.as_ref().is_some_and(|review| review.id == review_id);
            if current {
                rt.confirmed = Some(Confirmed { review_id: review_id.to_string(), acknowledged });
                rt.waiting_for = Some(who.to_string());
            }
            current
        }) == Some(true);
        if queued {
            self.inner.journal.info(id, &format!("confirmed by you; waits for “{who}” to finish syncing these folders"));
            self.publish(id);
        }
        queued
    }

    /// The confirmed list is being carried out: it no longer waits for an OK.
    pub(crate) fn take_review(&self, id: &JobId, review_id: &str) {
        self.change_known_runtime(id, |rt| {
            if rt.review.as_ref().is_some_and(|review| review.id == review_id) {
                rt.forget_review();
            }
        });
    }

    /// Carries out exactly the reviewed list, after the checks that make sure it can still be
    /// trusted. A list that `waited` for another sync was confirmed in time: neither its age
    /// nor the changes made in the folders since count against it (each step still checks
    /// its items right before it acts, and leaves alone what changed; an app library or a
    /// database is only carried when nothing of it changed, see `run::Executor`).
    fn carry_out(&self, id: &JobId, review_id: &str, acknowledged: bool, waited: bool) -> Result<()> {
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
        let mut reason = match review.kind {
            ReviewKind::Sync => "confirmed by you".to_string(),
            ReviewKind::Resolve => format!("to settle a conflict ({})", review.title.as_deref().unwrap_or("confirmed by you")),
        };
        if waited {
            reason.push_str(", after waiting for another sync");
        }
        let mut crossing =
            Crossing { kind: review.kind, approved: true, by_hand: true, reason, resolving: review.resolving.clone(), outdated: false };
        let (job, cancel) = self.begin_run(id, &crossing, |job, rt| {
            // Stopped, paused or edited while it waited: the person called it off, even if
            // that came after `start_confirmed` picked it up.
            if waited && !rt.confirmed.as_ref().is_some_and(|confirmed| confirmed.review_id == review_id) {
                return Err(Error::Cancelled);
            }
            let current = rt.review.as_ref().filter(|r| r.id == review_id).ok_or_else(Self::gone)?;
            if !waited && Utc::now() > current.expires_at {
                return Err(Error::ReviewOutdated(format!("This list was made more than {VALID_FOR_MINUTES} minutes ago. Look again.")));
            }
            if !waited && current.stale.load(Ordering::Relaxed) {
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
                let tick = if current.steps.is_empty() { "I've read the warnings above" } else { "I've looked at the removals and replacements" };
                return Err(Error::Invalid(format!("Tick “{tick}” first.")));
            }
            Ok(())
        })?;
        // A list that waited is older than the folders: the sync it waited for changed them,
        // and Charon may not have heard of it (nobody hears what another computer writes to a
        // network share). Those changes didn't count against the list, and aren't in it: once
        // it's done, the folders are looked at again.
        crossing.outdated = waited;
        // The run's busy notes go into the folders before anything changes there, and before
        // the person is told it started.
        let claim = match self.claim_folders(&job, &review.survey) {
            Ok(claim) => claim,
            Err(who) => return Err(if self.not_begun(&job, &crossing, &who) { Error::FoldersBusy(who) } else { Error::Cancelled }),
        };
        self.publish(id);
        self.spawn_crossing(job, cancel, crossing, move |engine, job, cancel, crossing| {
            engine.take_review(&job.id, &review.id);
            engine.cross(&job, &review.survey, &cancel, &crossing, claim);
        })
    }

    /// Prepares the list that settles one conflict the way the person chose. Nothing
    /// changes until it's confirmed like any other list.
    pub fn prepare_resolution(&self, id: &JobId, conflict: &ConflictItem, choice: ConflictChoice) -> Result<Review> {
        let (job, stop, epoch) = self.begin_look(id)?;
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
            self.note_look(&job, &survey, Some(epoch));
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
        // Folders one inside the other can't settle anything either, whether their paths
        // show it or only Charon's markers do.
        survey.refusal = self.overlaps(&job, &survey.source, &survey.destination).refusal;
        survey.holds.clear();
        survey.needs_tick = false;
        survey.agreed = 0;
        survey.housekeeping = Default::default();
        // What's said about the drives themselves still holds (another drive, a missing marker).
        survey.warnings.retain(|warning| warning.level == Level::Danger);
        if survey.memory.as_ref().is_some_and(|memory| memory.distrust.is_some()) {
            survey.warnings.push(Warning::info(
                "Charon has no memory of the last sync that it can trust right now, and settling this item doesn't change that: \
                 until you confirm a whole list of this job's changes, it compares both folders afresh and removes nothing.",
            ));
        }
        survey.warnings.push(Warning::info(
            "Only this item changes. The version that is replaced or removed goes to .charon-trash, even when safe deletes are off.",
        ));
        let review = self.keep_review(&job, survey, ReviewKind::Resolve, Some(title), Some(key.to_string()), epoch)?;
        self.publish(id);
        Ok(review)
    }

    // ---------------------------------------------------------------- internals

    /// Marks a look (a review being prepared) as started, if the job isn't busy. Returns the
    /// job, the look's stop flag, and the job's epoch when it began (see `note_look`).
    fn begin_look(&self, id: &JobId) -> Result<(crate::model::Job, Arc<AtomicBool>, u64)> {
        // Both ask the disk, so they come before any lock is taken.
        let neighbours = self.neighbours(id);
        let in_use = self.folders_in_use(id);
        let state = self.state();
        let job = state.jobs.iter().find(|job| &job.id == id).cloned().ok_or_else(|| Error::UnknownJob(id.clone()))?;
        let mut runtime = self.runtime();
        if self.is_shutting_down() {
            return Err(Error::Invalid("Charon is quitting.".into()));
        }
        if runtime.get(id).is_some_and(|rt| rt.is_running()) {
            return Err(Error::Busy);
        }
        if let Some(other) = Self::busy_neighbour(&state.jobs, &runtime, &job, &neighbours).or(in_use) {
            return Err(Error::FoldersBusy(other));
        }
        let rt = runtime.entry(id.clone()).or_default();
        if rt.preparing.is_some() {
            return Err(Error::Invalid("Charon is already looking at this job's folders.".into()));
        }
        // A background look at the same folders would only read them twice, and what it
        // found would be older than what this look finds.
        rt.outdate_looks();
        let stop = Arc::new(AtomicBool::new(false));
        rt.preparing = Some(stop.clone());
        Ok((job, stop, rt.epoch))
    }

    fn end_look(&self, id: &JobId, stop: &Arc<AtomicBool>) {
        self.change_known_runtime(id, |rt| {
            if rt.preparing.as_ref().is_some_and(|flag| Arc::ptr_eq(flag, stop)) {
                rt.preparing = None;
            }
        });
    }

    /// A list made while the job synced, changed or was removed, or while a folder came back.
    fn overtaken() -> Error {
        Error::ReviewOutdated(
            "The job or its folders changed while this list was being made (a sync, an edit, or a drive that came back). Look again.".into(),
        )
    }

    /// A conflict as the window shows it, saying when it's about a whole app library or
    /// database.
    fn conflict_item(survey: &Survey, key: &str, source: Option<Entry>, destination: Option<Entry>) -> ConflictItem {
        let unit = Unit::of(key, &survey.source, &survey.destination).filter(|unit| matches!(unit, Unit::Database(_)) || unit.key() == key);
        ConflictItem::new(key, &survey.spelling(key), source, destination).whole(unit.as_ref())
    }

    fn gone() -> Error {
        Error::ReviewOutdated("This list is no longer current. Look again.".into())
    }

    fn stored_review(&self, id: &JobId, review_id: &str) -> Result<Arc<StoredReview>> {
        self.runtime().get(id).and_then(|rt| rt.review.clone()).filter(|review| review.id == review_id).ok_or_else(Self::gone)
    }

    /// Keeps a review of `survey` for the job (replacing any other, and calling off a confirmed
    /// one that waited, which the job's log says) and returns it. `epoch` is the job's epoch
    /// when the look began: a list the job outgrew meanwhile (it synced, changed or was
    /// removed) isn't kept.
    pub(crate) fn keep_review(
        &self,
        job: &crate::model::Job,
        survey: Survey,
        kind: ReviewKind,
        title: Option<String>,
        resolving: Option<String>,
        epoch: u64,
    ) -> Result<Review> {
        let prepared_at = Utc::now();
        let clearings = survey.plan.clearings();
        let steps: Vec<ReviewStep> = survey.plan.for_review().into_iter().map(|action| Self::step_of(&survey, action, kind, &clearings)).collect();
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
        let confirm_anyway = match kind {
            ReviewKind::Sync if steps.is_empty() => Self::confirm_anyway(job, &survey, self.must_ask(&job.id)),
            _ => None,
        };
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
            confirm_anyway,
            resolving,
            steps,
            notes,
            survey,
        });
        let review = self.review_of(job, &stored);
        // Returns whether it was kept, and whether that called off a confirmed list.
        let kept = self.change_known_runtime(&job.id, |rt| {
            if rt.epoch != epoch || rt.is_running() {
                return (false, false);
            }
            let called_off = rt.confirmed.is_some();
            rt.forget_review();
            rt.review = Some(stored);
            // The person is looking at the list: the next reminder can wait.
            rt.reminder.last = Some(Utc::now());
            rt.reminder.count = rt.reminder.count.max(1);
            (true, called_off)
        });
        match kept {
            Some((true, called_off)) => {
                if called_off {
                    self.note_called_off(&job.id, " (a newer list took its place)");
                }
                Ok(review)
            }
            Some((false, _)) => Err(Self::overtaken()),
            None => Err(Error::UnknownJob(job.id.clone())),
        }
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
            confirm_anyway: stored.confirm_anyway.clone(),
            clusters: Self::clusters_of(survey),
            housekeeping,
            total_steps: stored.steps.len(),
            total_notes: stored.notes.len(),
            steps: stored.steps.iter().take(FIRST_STEPS).cloned().collect(),
            notes: stored.notes.iter().take(FIRST_NOTES).cloned().collect(),
        }
    }

    /// Why a list without steps is still worth confirming, in a sentence for the list, or
    /// `None` when it would only say that both folders match. Confirming it changes no file,
    /// but it is a sync like any other: it ends a hold, lets full automation sync by itself
    /// again, and records both folders as they are now (their drives, Charon's markers and,
    /// in two-way, the memory that lets removals cross).
    fn confirm_anyway(job: &crate::model::Job, survey: &Survey, must_ask: bool) -> Option<String> {
        let held = job.held.is_some();
        let why = if job.setup.full_automation && (held || job.needs_review || must_ask) {
            "Confirm once so Charon may sync this job by itself from now on."
        } else if held {
            "Confirm it, and the job stops waiting for your OK."
        } else if survey.unfamiliar {
            "If these are the right folders, confirm it: Charon then takes them as they are now."
        } else if survey.setup.mode.is_two_way() && !survey.trusted {
            "Confirm it, and Charon remembers both folders as they are now, so that later lists can carry what you remove on one side to the other."
        } else if job.last_synced.is_none() {
            "Confirm it as the job's first sync, so Charon knows both folders from now on."
        } else {
            return None;
        };
        Some(why.into())
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

    /// One step as the review shows it, with why it happens. `clearings`: the plan's places
    /// where an item of another kind takes an item's place.
    fn step_of(survey: &Survey, action: &Action, kind: ReviewKind, clearings: &[Clearing]) -> ReviewStep {
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
                // What makes room for an item of another kind is kept whatever the setting
                // (see `Executor::delete`).
                let makes_room = clearings.iter().any(|clearing| clearing.holds(action));
                let fate = if kept || makes_room { "goes to .charon-trash" } else { "gone for good" };
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
                let unit = Unit::of(key, &survey.source, &survey.destination)
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
    /// have waited. `epoch` is the job's epoch when the look began: a look the job outgrew
    /// meanwhile (a run began, the job changed or was removed, a folder came back) is
    /// dropped, so it never takes the place of a newer one. A run's own look has none: no
    /// other look can begin while a run is going. Returns whether the look was recorded.
    pub(crate) fn note_look(&self, job: &crate::model::Job, survey: &Survey, epoch: Option<u64>) -> bool {
        let Ok(stored) = self.stored_job(&job.id) else { return false };
        // A look made for a setup that changed meanwhile tells nothing.
        if stored.setup != job.setup {
            return false;
        }
        let held = stored.held.is_some();
        let look = Look::of(survey);
        let at = look.at;
        let has_work = look.has_work();
        let needs_a_list = look.needs_a_list();
        let recorded = self.change_known_runtime(&job.id, |rt| {
            if epoch.is_some_and(|epoch| epoch != rt.epoch || rt.is_running()) {
                return false;
            }
            if !needs_a_list && !held {
                rt.reminder.count = 0;
            }
            rt.problem = None;
            rt.look = Some(look);
            // A list counts as a look: the next background look waits its turn.
            rt.last_checked = Some(Instant::now());
            true
        });
        if recorded != Some(true) {
            return false;
        }
        // Saved only when the wait begins or ends. Files not compared yet aren't changes known
        // to wait: after a restart, the row waits for a look to say what's in them.
        let since = self.stored_job(&job.id).ok().and_then(|stored| stored.waiting_since);
        let wanted = if has_work || held { since.or(Some(at)) } else { None };
        if wanted != since {
            if let Err(e) = self.change_job(&job.id, |stored| stored.waiting_since = wanted) {
                log::warn!("couldn't save since when changes of {} wait: {e}", job.setup.name);
            }
        }
        true
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
                .filter_map(|note| Some(crate::model::Place::new(note.kind, note.side?, note.path.clone(), note.reason.clone())))
                .collect()
        };
        let mut unreadable = places(&[NoteKind::Unreadable]);
        let unreadable_count = unreadable.len() as u64;
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
            unreadable_count,
            not_synced,
            not_synced_count,
            volumes,
            uncompared: survey.uncompared.len() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConflictRule, JobSetup, SyncMode, Trigger};
    use std::fs;
    use std::thread;

    #[test]
    fn a_confirmed_sync_stopped_after_it_was_picked_up_never_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let (src, dst) = (tmp.path().join("src"), tmp.path().join("dst"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        let engine = Engine::open(tmp.path().join("data"), Arc::new(|_| {})).unwrap();
        let setup = JobSetup {
            name: "Photos".into(),
            source: src.clone(),
            destination: dst.clone(),
            mode: SyncMode::Backup,
            conflict: ConflictRule::Ask,
            trigger: Trigger::Manual,
            excludes: JobSetup::default_excludes(),
            safe_delete: true,
            full_automation: false,
        };
        let id = engine.add_job(setup).unwrap().job.id;
        let review = engine.prepare_review(&id).unwrap();
        // Confirmed while another sync used the folders, as `queue_confirmed` keeps it.
        let waiting = Confirmed { review_id: review.review_id.clone(), acknowledged: true };
        engine.change_runtime(&id, |rt| rt.confirmed = Some(waiting.clone()));

        // `start_confirmed` picks it up, and the person presses Stop before its sync begins.
        let picked = engine.runtime().get(&id).and_then(|rt| rt.confirmed.clone()).unwrap();
        engine.cancel(&id);
        assert!(matches!(engine.carry_out(&id, &picked.review_id, picked.acknowledged, true), Err(Error::Cancelled)));
        assert!(!engine.is_running(&id));
        assert!(!dst.join("a.txt").exists(), "nothing ran");

        // The same list, not called off, starts.
        engine.change_runtime(&id, |rt| rt.confirmed = Some(waiting.clone()));
        engine.carry_out(&id, &waiting.review_id, waiting.acknowledged, true).unwrap();
        while engine.is_running(&id) {
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(dst.join("a.txt").exists());
    }
}
