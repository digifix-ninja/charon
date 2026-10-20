//! A survey: both folders as they are now, what it takes to bring them together, and
//! everything about that the person (or a run nobody looks at) must know first.
//!
//! Every look goes through here: the background checks, the reviews the person confirms,
//! and the runs of jobs with full automation. Nothing in a survey changes a file.

use crate::engine::{runs, Engine, EngineEvent};
use crate::error::{Error, Result};
use crate::model::{human_bytes, human_count, plural, Job, JobSetup, Warning};
use crate::plan::{self, Action, Baseline, Guard, NoteKind, OneWayPlanner, Pair, Plan, Recent, Side, Unit};
use crate::run::{Found, Markers, Room, Trash, TrashLook};
use crate::scan::{never_download_on_this_thread, partial_started, Contents, Entry, EntryKind, ExcludeFilter, FsKind, Keys, Scanner, Snapshot, Volume};
use crate::store::Memory;
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;

/// Half-copied files from an interrupted sync are tidied away once they're this old (a
/// younger one may belong to a sync running right now in another job).
const LEFTOVER_AGE_MS: i64 = 60 * 60 * 1000;
/// A background look compares at most this much by contents; whatever is left counts as
/// identical for that look only (a list the person sees, and every run, read it all).
const CHECK_COMPARE_BYTES: u64 = 1 << 30;
/// Comparison progress reaches the window at most this often.
const COMPARE_PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// Why a survey is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    /// A background look for changes.
    Check,
    /// A list the person will look at.
    Review,
    /// A run nobody looks at (full automation).
    Automatic,
}

/// What a sync also does besides its steps, on the sides it writes to.
#[derive(Debug, Clone, Default)]
pub(crate) struct Housekeeping {
    /// What each side's trash holds: the dated folders old enough to be emptied, and files
    /// changed after they went in.
    pub trash: Vec<(Side, TrashLook)>,
    /// Half-copied files left by an interrupted sync, per side (on-disk path, item).
    pub leftovers: Vec<(Side, Vec<(String, Entry)>)>,
}

impl Housekeeping {
    /// The lines a review shows.
    pub fn lines(&self, trash_days: u32) -> Vec<String> {
        let mut lines = Vec::new();
        for (side, look) in &self.trash {
            let expired = &look.expired;
            if expired.is_empty() {
                continue;
            }
            let items: u64 = expired.iter().map(|e| e.items).sum();
            let bytes: u64 = expired.iter().map(|e| e.bytes).sum();
            lines.push(format!(
                "Also empties {} older than {} from .charon-trash on the {} ({}).",
                plural(items, "item"),
                plural(trash_days as u64, "day"),
                side.word(),
                human_bytes(bytes)
            ));
        }
        for (side, leftovers) in &self.leftovers {
            if !leftovers.is_empty() {
                lines.push(format!(
                    "Also moves {} left by an interrupted sync into .charon-trash on the {}.",
                    plural(leftovers.len() as u64, "half-copied file"),
                    side.word()
                ));
            }
        }
        lines
    }
}

/// A pair of files whose contents are read (see `Engine::compare_contents`).
struct Candidate {
    key: String,
    source: Entry,
    destination: Entry,
    /// Both sides changed since the last sync and look alike by size and date.
    alike: bool,
}

/// What reading contents found.
#[derive(Default)]
struct Compared {
    /// Pairs found identical, now recorded as in sync.
    agreed: u64,
    /// Pairs that both changed and look alike, but whose contents differ.
    different: BTreeSet<String>,
}

/// Both folders now, the plan, and what deserves a look.
#[derive(Debug, Clone)]
pub(crate) struct Survey {
    /// The job's setup the survey was made with.
    pub setup: JobSetup,
    pub source: Snapshot,
    pub destination: Snapshot,
    /// Whether keys ignore upper and lower case (one of the drives does).
    pub folded: bool,
    /// The job's markers in both folders, read before the scans. A sync records nothing
    /// unless they are still the same when it's done.
    pub markers: Markers,
    /// Two-way: the memory as loaded (trusted or not).
    pub memory: Option<Memory>,
    /// Two-way: the plan was made with the stored memory.
    pub trusted: bool,
    /// The baseline the plan was made with (the trusted memory, plus pairs found identical).
    pub baseline: Baseline,
    /// Mirror and Backup: the copies whose dates didn't stick that the plan was made with.
    pub landed: Baseline,
    pub plan: Plan,
    /// Pairs of files found identical by their contents, though their dates differ.
    pub agreed: u64,
    /// Why this plan must not run at all.
    pub refusal: Option<String>,
    pub warnings: Vec<Warning>,
    /// Why a run nobody looks at must not go ahead.
    pub holds: Vec<String>,
    /// Confirming needs an explicit tick.
    pub needs_tick: bool,
    pub housekeeping: Housekeeping,
    pub made_at: DateTime<Utc>,
}

impl Survey {
    pub fn volume(&self, side: Side) -> Option<&Volume> {
        self.snapshot(side).volume()
    }

    pub fn snapshot(&self, side: Side) -> &Snapshot {
        match side {
            Side::Source => &self.source,
            Side::Destination => &self.destination,
        }
    }

    /// The path of `key` as the person sees it: as spelled on the side that has it.
    pub fn spelling(&self, key: &str) -> String {
        if self.source.contains(key) || self.source.has_spelling(key) {
            self.source.spelling(key).to_string()
        } else {
            self.destination.spelling(key).to_string()
        }
    }

    /// The sides a sync of this job writes to.
    pub fn written_sides(setup: &JobSetup) -> &'static [Side] {
        if setup.mode.is_two_way() {
            &[Side::Source, Side::Destination]
        } else {
            &[Side::Destination]
        }
    }
}

impl Engine {
    /// Scans both folders of `job` and plans its next sync, with every check that goes with
    /// it. `cancel` stops the scans.
    pub(crate) fn survey(&self, job: &Job, cancel: &AtomicBool, purpose: Purpose) -> Result<Survey> {
        let setup = job.setup.clone();
        for root in [&setup.source, &setup.destination] {
            if !root.is_dir() {
                return Err(Error::Offline(root.clone()));
            }
        }
        let markers = Markers::read(&setup.source, &setup.destination, &job.id);
        let volumes = (Volume::of(&setup.source), Volume::of(&setup.destination));
        let folded = !(volumes.0.case_sensitive && volumes.1.case_sensitive);
        let filter = ExcludeFilter::new(&setup.excludes, folded)?;
        let (mut source, mut destination) = self.scan_both(job, &filter, volumes.clone(), cancel, purpose != Purpose::Check)?;
        if folded {
            source.fold();
            destination.fold();
        }
        let (no_source_links, no_destination_links) = self.change_runtime(&job.id, |rt| rt.no_links);
        if no_source_links {
            source.refuse_links();
        }
        if no_destination_links {
            destination.refuse_links();
        }

        let mut warnings = Vec::new();
        let mut holds = Vec::new();
        let mut needs_tick = false;
        // Why the job waits, where this look can't tell by itself.
        match job.held.as_deref() {
            Some(held) if purpose == Purpose::Review && held == runs::STOPPED => warnings.push(Warning::info(held)),
            Some(held) if purpose == Purpose::Review && held == runs::UNRECORDED => warnings.push(Warning::caution(held)),
            _ => {}
        }

        // Two-way: what Charon remembers of the last sync, if it can be trusted.
        let (memory, trusted, mut baseline) = if setup.mode.is_two_way() {
            let memory = self.inner.baselines.load(&job.id, &setup);
            match self.distrust(job, &memory, folded, &markers) {
                Some(reason) => {
                    let text = format!(
                        "{reason} So this time Charon compares both folders afresh: it removes nothing, so items removed since the last sync may come back, and files that differ are kept twice or left for you."
                    );
                    holds.push(text.clone());
                    warnings.push(Warning::caution(text));
                    (Some(memory), false, Baseline::new())
                }
                None if memory.tokens.is_none() => {
                    warnings.push(Warning::info(
                        "This is the first two-way sync of these folders (or the first since the job's folders or mode changed). \
                         Charon compares both and removes nothing: items only on one side are copied to the other, and files that differ are kept twice or left for you.",
                    ));
                    if job.last_synced.is_some() {
                        holds.push("Charon has no memory of this job's last sync, so it compares both folders afresh.".into());
                    }
                    (Some(memory), false, Baseline::new())
                }
                None => {
                    let baseline = memory.baseline.clone();
                    (Some(memory), true, baseline)
                }
            }
        } else {
            (None, false, Baseline::new())
        };
        // Mirror and Backup: copies whose dates didn't stick on the destination's drive.
        let landed = if setup.mode.is_two_way() {
            Baseline::new()
        } else {
            self.inner.landings.load(&job.id, &setup, folded, &markers.destination)
        };
        let compared = if setup.mode.is_two_way() {
            self.compare_contents(job, &source, &destination, &mut baseline, purpose, cancel)?
        } else {
            Compared::default()
        };
        let agreed = compared.agreed;

        let record = if setup.mode.is_two_way() { &baseline } else { &landed };
        let plan = plan::plan_for(&setup, &source, &destination, record, &compared.different);
        let refusal = Guard::refusal(&plan, &source, &destination);
        if let Some(refusal) = &refusal {
            holds.push(refusal.clone());
        }

        // What automatic syncs removed or replaced today counts too: for the runs nobody
        // looks at, and in the list of a job that has them, so it shows why a run was held.
        let recent = (purpose == Purpose::Automatic || (purpose == Purpose::Review && setup.full_automation)).then(|| {
            let (source, destination) = job.recent_automatic(Utc::now());
            Recent { source, destination }
        });
        for concern in Guard::concerns(&plan, &source, &destination, &baseline, recent.as_ref()) {
            holds.push(concern.clone());
            warnings.push(Warning::danger(concern));
            needs_tick = true;
        }

        // The folders must be on the drives they were on at the last sync.
        if let Some(drives) = &job.drives {
            for (side, before, now) in [(Side::Source, &drives.source, &volumes.0), (Side::Destination, &drives.destination, &volumes.1)] {
                if !Self::same_drive(before, now) {
                    let text = format!(
                        "The {} folder is now on {}, but at the last sync it was on {}. If that drive isn't connected, this may be a leftover folder on another disk.",
                        side.word(),
                        now.describe(),
                        before.describe()
                    );
                    holds.push(text.clone());
                    warnings.push(Warning::danger(text));
                    needs_tick = true;
                }
            }
        }
        // Mirror and Backup: both folders must carry the markers of the last sync.
        if !setup.mode.is_two_way() {
            let concerns = [Self::destination_concern(job, &markers.destination), Self::source_concern(job, &markers.source)];
            for text in concerns.into_iter().flatten() {
                holds.push(text.clone());
                warnings.push(Warning::danger(text));
                needs_tick = true;
            }
        }

        let removals = plan.removed_on(Side::Source) + plan.removed_on(Side::Destination);
        let replacements: u64 = plan
            .actions
            .iter()
            .filter(|a| matches!(a, Action::Copy { replaces: true, item, .. } if *item != EntryKind::Dir))
            .count() as u64;
        if !setup.safe_delete && removals + replacements > 0 {
            let text = if setup.mode.is_two_way() || replacements == 0 {
                "Safe deletes are off for this job: removed files are gone for good, with no copy in .charon-trash."
            } else {
                "Safe deletes are off for this job: removed and replaced files are gone for good, with no copy in .charon-trash."
            };
            warnings.push(Warning::caution(text));
            needs_tick = needs_tick || removals > 0 || !setup.mode.is_two_way();
        }
        let unreadable = plan.notes.iter().filter(|n| n.kind == NoteKind::Unreadable).count() as u64;
        if unreadable > 0 {
            warnings.push(Warning::caution(format!(
                "Charon couldn't read {}, so nothing in {} is synced. {} listed in the notes.",
                plural(unreadable, "place"),
                if unreadable == 1 { "it" } else { "them" },
                if unreadable == 1 { "It's" } else { "They're" }
            )));
        }
        let left_alone = plan.notes.iter().filter(|n| matches!(n.kind, NoteKind::LeftOut | NoteKind::NameClash | NoteKind::Blocked)).count() as u64;
        if left_alone > 0 {
            warnings.push(Warning::caution(format!(
                "{} can't be synced (a name or size the other drive can't hold, two names that clash, a file meeting a folder…). {} listed in the notes, with the reason.",
                if left_alone == 1 { "1 item".to_string() } else { format!("{} items", human_count(left_alone)) },
                if left_alone == 1 { "It's" } else { "They're" }
            )));
        }
        let settled = plan.summary().newest_wins;
        if settled > 0 {
            warnings.push(Warning::info(format!(
                "{} settled by Newest wins: the version saved last replaces the other, and the replaced version goes to .charon-trash. This relies on both computers' clocks.",
                if settled == 1 { "1 conflict is".to_string() } else { format!("{} conflicts are", human_count(settled)) }
            )));
        }
        let undated = OneWayPlanner::backup().knowing_landed(landed.clone()).landed_alike(&source, &destination);
        if undated > 0 {
            warnings.push(Warning::info(format!(
                "{} in the destination {} other dates than in the source, because the destination's drive doesn't keep the dates Charon sets. \
                 {} copied in full and neither side changed since, so Charon leaves {} alone.",
                plural(undated, "file"),
                if undated == 1 { "has" } else { "have" },
                if undated == 1 { "It was" } else { "They were" },
                if undated == 1 { "it" } else { "them" }
            )));
        }
        if let Some(text) = Self::units_warning(&setup, &plan, &source, &destination) {
            warnings.push(Warning::caution(text));
        }
        let links = plan.actions.iter().filter(|a| matches!(a, Action::Copy { item: EntryKind::Link, .. })).count() as u64;
        if links > 0 {
            warnings.push(Warning::info(format!(
                "{} copied as {}, not the files {} point to.",
                if links == 1 { "1 symbolic link is".to_string() } else { format!("{} symbolic links are", human_count(links)) },
                if links == 1 { "a link" } else { "links" },
                if links == 1 { "it" } else { "they" }
            )));
        }

        let housekeeping = self.housekeeping(&setup, &source, &destination, purpose);
        for (side, look) in &housekeeping.trash {
            if let Some(text) = Self::revived_warning(*side, look) {
                holds.push(text.clone());
                warnings.push(Warning::caution(text));
            }
            if purpose != Purpose::Check {
                if let Some(text) = self.trash_size_warning(&setup, *side, look) {
                    warnings.push(Warning::info(text));
                }
            }
        }
        for &side in Survey::written_sides(&setup) {
            let clones = match side {
                Side::Source => &source,
                Side::Destination => &destination,
            }
            .volume()
            .is_some_and(|v| v.kind == FsKind::Apfs);
            let bytes = Self::bytes_onto(&setup, &plan, side, clones);
            if bytes == 0 {
                continue;
            }
            if let Some(why) = Room::shortage(setup.root(side), bytes, &format!("the {}'s drive", side.word())) {
                let text = format!(
                    "{}{}. Copies that don't fit will fail, and while copies to the {} fail, nothing is removed there. Make room on the drive first.",
                    why[..1].to_uppercase(),
                    &why[1..],
                    side.word()
                );
                holds.push(text.clone());
                warnings.push(Warning::caution(text));
            }
        }
        Ok(Survey {
            setup,
            source,
            destination,
            folded,
            markers,
            memory,
            trusted,
            baseline,
            landed,
            plan,
            agreed,
            refusal,
            warnings,
            holds,
            needs_tick,
            housekeeping,
            made_at: Utc::now(),
        })
    }

    /// App libraries and databases this sync changes: copied while their app writes to them,
    /// they may arrive half-written.
    fn units_warning(setup: &JobSetup, plan: &Plan, source: &Snapshot, destination: &Snapshot) -> Option<String> {
        let exists = |key: &str| source.contains(key) || destination.contains(key);
        let units: BTreeSet<Unit> = plan.work().filter_map(|action| Unit::of(action.path(), exists)).collect();
        let names: Vec<String> = units
            .iter()
            .map(|unit| {
                let side = if source.contains(unit.key()) { source } else { destination };
                format!("“{}”", side.spelling(unit.key()))
            })
            .collect();
        let computers = if setup.mode.is_two_way() { " (on both computers)" } else { "" };
        match names.as_slice() {
            [] => None,
            [name] => Some(format!(
                "{name} is {}. Close the app that uses it before you confirm{computers}, or it may be copied while it's half-written.",
                units.first().map_or("", Unit::describe)
            )),
            _ => {
                let shown = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
                let more = if names.len() > 3 { format!(" and {} more", human_count(names.len() as u64 - 3)) } else { String::new() };
                Some(format!(
                    "{} app libraries or databases change in this sync: {shown}{more}. Close the apps that use them before you confirm{computers}, or they may be copied while they're half-written.",
                    human_count(names.len() as u64)
                ))
            }
        }
    }

    /// Scans both folders at once (they're usually on different drives), each on its own
    /// thread, which never downloads files that are only in the cloud.
    fn scan_both(&self, job: &Job, filter: &ExcludeFilter, volumes: (Volume, Volume), cancel: &AtomicBool, report: bool) -> Result<(Snapshot, Snapshot)> {
        let (source_volume, destination_volume) = volumes;
        let scan = |side: Side, volume: Volume| {
            never_download_on_this_thread();
            let counter = |items: u64| {
                if report {
                    self.emit(EngineEvent::ScanProgress { id: job.id.clone(), side, items });
                }
            };
            let root = match side {
                Side::Source => &job.setup.source,
                Side::Destination => &job.setup.destination,
            };
            Scanner::new(filter).cancellable(cancel).counting(&counter).scan(root, volume)
        };
        let (source, destination) = std::thread::scope(|scope| {
            let source = std::thread::Builder::new().name("charon-scan".into()).spawn_scoped(scope, || scan(Side::Source, source_volume));
            let destination = scan(Side::Destination, destination_volume);
            let source = match source {
                Ok(handle) => handle.join().unwrap_or_else(|_| Err(Error::Invalid("The scan of the source stopped unexpectedly.".into()))),
                Err(e) => Err(Error::Io(e)),
            };
            (source, destination)
        });
        Ok((source?, destination?))
    }

    /// Mirror and Backup: why the destination may not be the folder of the last sync.
    fn destination_concern(job: &Job, found: &Found) -> Option<String> {
        let token = job.destination_token.as_ref()?;
        if matches!(found, Found::Marker(marker) if &marker.token == token) {
            return None;
        }
        Some("The destination doesn't carry the marker Charon left there at the last sync. It may be another drive or folder under the same name.".into())
    }

    /// Mirror and Backup: why the source may not be the folder of the last sync. An older
    /// copy of the source would make the sync remove or replace the newest backups.
    fn source_concern(job: &Job, found: &Found) -> Option<String> {
        let mark = job.source_mark.as_ref()?;
        let text = match found {
            Found::Marker(marker) if marker.token == mark.token && marker.generation == mark.generation => return None,
            Found::Marker(marker) if marker.token == mark.token && marker.generation < mark.generation => {
                "The source went back in time since the last sync: it may be an older copy of the drive, restored from a backup. \
                 Syncing it could replace newer files in the destination with older ones, or remove the newest."
            }
            Found::Marker(marker) if marker.token == mark.token => {
                "The source was synced after Charon last saved its record of this job (was Charon's own data restored from a backup?)."
            }
            Found::Marker(_) => "The source carries another sync's marker, so it may not be the folder Charon synced from (another drive with the same name?).",
            Found::Missing => {
                "The source doesn't carry the marker Charon left there at the last sync. It may be another drive or folder under the same name, or an older copy of it."
            }
            Found::Unusable(_) => "Charon's marker in the source can't be read, so it can't tell whether this is the folder it synced from.",
        };
        Some(text.into())
    }

    /// Why the stored two-way memory can't be trusted for these folders, if it can't.
    fn distrust(&self, job: &Job, memory: &Memory, folded: bool, markers: &Markers) -> Option<String> {
        if let Some(reason) = &memory.distrust {
            return Some(reason.clone());
        }
        if job.run_in_progress {
            return Some("The last sync of this job didn't finish properly (Charon was closed, the computer stopped, or a drive went away).".into());
        }
        // No memory yet (nothing to trust or distrust): nothing is removed.
        memory.tokens.as_ref()?;
        if memory.folded != folded {
            return Some("One of the drives treats upper and lower case differently than at the last sync.".into());
        }
        for side in [Side::Source, Side::Destination] {
            let expected = memory.token(side).unwrap_or_default();
            let problem = match markers.side(side) {
                Found::Marker(marker) if marker.token != expected => {
                    format!("The {} folder carries another sync's marker, so it may not be the folder Charon synced with (another drive with the same name?).", side.word())
                }
                Found::Marker(marker) if marker.generation < memory.generation => format!(
                    "The {} folder went back in time since the last sync (restored from a copy, or the last sync's changes never reached the drive).",
                    side.word()
                ),
                Found::Marker(marker) if marker.generation > memory.generation => {
                    format!("The {} folder was synced after Charon last saved what it remembers.", side.word())
                }
                Found::Marker(_) => continue,
                Found::Missing => format!(
                    "The {} folder doesn't carry Charon's marker from the last sync: it may be another folder or drive under the same name, or the marker was removed.",
                    side.word()
                ),
                Found::Unusable(reason) => format!("Charon's marker in the {} folder can't be read ({reason}).", side.word()),
            };
            return Some(problem);
        }
        None
    }

    /// Pairs of files whose contents decide what the plan does, read byte for byte:
    ///
    /// * no record of the last sync (or both sides changed since), the same size but
    ///   different dates: identical pairs are recorded as in sync;
    /// * one side changed since the record but kept its size, and only its details or its
    ///   time moved, by a rounding or by quarter hours (a tag or permission change, a drive
    ///   that shifts or rounds times): identical to the other side, the record is renewed;
    /// * both sides changed since the record and still look alike: if their contents differ
    ///   after all, they're a conflict (`different`), never "already in sync".
    ///
    /// A background look reads at most `CHECK_COMPARE_BYTES`; the rest count as identical
    /// for that look only.
    fn compare_contents(
        &self,
        job: &Job,
        source: &Snapshot,
        destination: &Snapshot,
        baseline: &mut Baseline,
        purpose: Purpose,
        cancel: &AtomicBool,
    ) -> Result<Compared> {
        let candidates: Vec<Candidate> = source.iter().filter_map(|(key, s)| Self::candidate(key, s, source, destination, baseline)).collect();
        let total_bytes: u64 = candidates.iter().map(|c| c.source.size).sum();
        let total_files = candidates.len() as u64;
        let mut compared = Compared::default();
        let (mut done_files, mut done_bytes, mut read_bytes) = (0u64, 0u64, 0u64);
        let mut last_sent: Option<std::time::Instant> = None;
        for candidate in candidates {
            let Candidate { key, source: s, destination: d, alike } = candidate;
            let cached = self.change_runtime(&job.id, |rt| rt.same_contents.get(&key).filter(|(cs, cd, _)| *cs == s && *cd == d).map(|(_, _, same)| *same));
            let same = match cached {
                Some(same) => Some(same),
                // A background look that has read enough leaves the rest for the next list.
                None if purpose == Purpose::Check && read_bytes + s.size > CHECK_COMPARE_BYTES => None,
                None => {
                    let a = job.setup.source.join(source.spelling(&key));
                    let b = job.setup.destination.join(destination.spelling(&key));
                    read_bytes += s.size;
                    let same = match Contents::same(&a, &b, Some(cancel)) {
                        Ok(same) => same,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err(Error::Cancelled),
                        // Unreadable now: the plan decides as if they differ (never a removal).
                        Err(_) => false,
                    };
                    self.change_runtime(&job.id, |rt| {
                        rt.same_contents.insert(key.clone(), (s, d, same));
                    });
                    Some(same)
                }
            };
            match same {
                Some(true) | None => {
                    baseline.insert(key, Pair::new(s, d));
                    compared.agreed += 1;
                }
                Some(false) if alike => {
                    compared.different.insert(key);
                }
                Some(false) => {}
            }
            done_files += 1;
            done_bytes += s.size;
            if purpose != Purpose::Check && last_sent.is_none_or(|t| t.elapsed() >= COMPARE_PROGRESS_EVERY || done_files == total_files) {
                last_sent = Some(std::time::Instant::now());
                self.emit(EngineEvent::CompareProgress { id: job.id.clone(), files: done_files, total_files, bytes: done_bytes, total_bytes });
            }
        }
        Ok(compared)
    }

    /// Whether the pair at `key` needs its contents read (see `compare_contents`).
    fn candidate(key: &str, s: &Entry, source: &Snapshot, destination: &Snapshot, baseline: &Baseline) -> Option<Candidate> {
        let d = destination.get(key)?;
        if !(s.is_file() && d.is_file() && s.size == d.size) || source.frozen(key).is_some() || destination.frozen(key).is_some() {
            return None;
        }
        let wanted = match baseline.get(key) {
            None => !s.same_as(d),
            Some(pair) => {
                let source_still = Entry::still_on(Some(s), Some(&pair.source), source.shifts_times());
                let destination_still = Entry::still_on(Some(d), Some(&pair.destination), destination.shifts_times());
                match (source_still, destination_still) {
                    (true, true) => false,
                    (false, false) => true,
                    (false, true) => s.drifted_from(&pair.source),
                    (true, false) => d.drifted_from(&pair.destination),
                }
            }
        };
        let alike = baseline.get(key).is_some() && s.same_as(d);
        wanted.then(|| Candidate { key: key.to_string(), source: *s, destination: *d, alike })
    }

    /// Old trash to empty and half-copied leftovers to tidy, on the sides the job writes to.
    /// A background look only looks into the trash folders old enough to be emptied.
    fn housekeeping(&self, setup: &JobSetup, source: &Snapshot, destination: &Snapshot, purpose: Purpose) -> Housekeeping {
        let trash_days = self.settings().trash_days;
        let now = Utc::now().timestamp_millis();
        let mut housekeeping = Housekeeping::default();
        for &side in Survey::written_sides(setup) {
            let (root, snapshot) = match side {
                Side::Source => (&setup.source, source),
                Side::Destination => (&setup.destination, destination),
            };
            housekeeping.trash.push((side, Trash::look(root, trash_days, purpose != Purpose::Check)));
            // Old by its time and by when its copy started: a long copy carries the old time
            // of its original at the end, and may belong to another computer's sync.
            let old = |path: &str, entry: &Entry| {
                now - entry.modified > LEFTOVER_AGE_MS && partial_started(Keys::name(path), now).is_none_or(|started| now - started > LEFTOVER_AGE_MS)
            };
            let leftovers: Vec<(String, Entry)> = snapshot.partials().iter().filter(|(path, entry)| old(path, entry)).cloned().collect();
            housekeeping.leftovers.push((side, leftovers));
        }
        housekeeping
    }

    /// Files saved into the trash after Charon put them there: an app that had a file open
    /// may have followed it into the trash and gone on saving someone's work there.
    fn revived_warning(side: Side, look: &TrashLook) -> Option<String> {
        let count = look.revived.len() as u64;
        if count == 0 {
            return None;
        }
        let one = count == 1;
        let shown: Vec<String> = look.revived.iter().take(3).map(|path| format!("“{path}”")).collect();
        let more = if count > 3 { format!(", and {} more", human_count(count - 3)) } else { String::new() };
        Some(format!(
            "{} in .charon-trash on the {} changed after Charon put {} there: an app that had {} open may have saved someone's work into {}. \
             Charon won't empty {}; move what you need back, or delete {} yourself: {}{more}.",
            plural(count, "file"),
            side.word(),
            if one { "it" } else { "them" },
            if one { "it" } else { "them" },
            if one { "it" } else { "them" },
            if one { "it" } else { "them" },
            if one { "it" } else { "them" },
            shown.join(", ")
        ))
    }

    /// A trash that takes a large share of its drive.
    fn trash_size_warning(&self, setup: &JobSetup, side: Side, look: &TrashLook) -> Option<String> {
        let room = Room::of(setup.root(side))?;
        if look.bytes < 1_000_000_000 || look.bytes < room.total / 10 {
            return None;
        }
        let days = self.settings().trash_days;
        let emptied = if days == 0 {
            "Settings keep removed files there for good".to_string()
        } else {
            format!("Charon empties what's older than {}", plural(days as u64, "day"))
        };
        Some(format!(
            ".charon-trash on the {} holds {} ({}% of its drive). {emptied}; to make room sooner, delete older dated folders from it yourself.",
            side.word(),
            human_bytes(look.bytes),
            look.bytes.saturating_mul(100) / room.total.max(1)
        ))
    }

    /// What a sync writes onto `side`'s drive: its copies there, and the copies of the
    /// versions they replace that go into .charon-trash (none where those are clones).
    fn bytes_onto(setup: &JobSetup, plan: &Plan, side: Side, clones: bool) -> u64 {
        let keeps_replaced = setup.safe_delete || setup.mode.is_two_way();
        plan.work()
            .map(|action| match action {
                Action::Copy { to, bytes, existing, settles_conflict, .. } if *to == side => {
                    let kept = match existing {
                        Some(old) if old.is_file() && !clones && (keeps_replaced || *settles_conflict) => old.size,
                        _ => 0,
                    };
                    bytes + kept
                }
                Action::KeepBoth { source_bytes, destination_bytes, .. } => match side {
                    Side::Destination => *source_bytes,
                    Side::Source => *destination_bytes,
                },
                _ => 0,
            })
            .fold(0u64, u64::saturating_add)
    }

    /// Whether a folder is on the same drive as before (unknown details never count as a change).
    pub(crate) fn same_drive(before: &Volume, now: &Volume) -> bool {
        let same_mount = before.mount.is_empty() || now.mount.is_empty() || before.mount == now.mount;
        same_mount && before.kind == now.kind
    }
}
