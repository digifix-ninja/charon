//! When a scheduled job runs next.
//!
//! Schedules count from the last run (or the job's creation), not from a timer, so a
//! computer that was asleep catches up as soon as it wakes.

use crate::model::{Job, Trigger};
use chrono::{DateTime, Duration, Local, TimeZone, Utc};

pub struct Schedule;

impl Schedule {
    /// The next automatic run of an interval or daily job.
    pub fn next_run(job: &Job) -> Option<DateTime<Utc>> {
        Self::next_from(job, job.schedule_reference())
    }

    fn next_from(job: &Job, reference: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match job.setup.trigger {
            Trigger::Interval { minutes } => Some(reference + Duration::minutes(minutes as i64)),
            Trigger::Daily { hour, minute } => Self::next_daily(reference, hour, minute),
            _ => None,
        }
    }

    /// Whether the job's schedule says it should run now.
    pub fn is_due(job: &Job, now: DateTime<Utc>) -> bool {
        Self::next_run(job).is_some_and(|next| now >= next)
    }

    /// `is_due`, also counting from `since` (when the schedule last came due, if that's
    /// later than the job's record says).
    pub fn is_due_after(job: &Job, since: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
        let reference = match since {
            Some(since) if since > job.schedule_reference() => since,
            _ => job.schedule_reference(),
        };
        Self::next_from(job, reference).is_some_and(|next| now >= next)
    }

    /// The first local `hour:minute` after `after`.
    fn next_daily(after: DateTime<Utc>, hour: u8, minute: u8) -> Option<DateTime<Utc>> {
        let after = after.with_timezone(&Local);
        let mut date = after.date_naive();
        for _ in 0..3 {
            let time = date.and_hms_opt(hour as u32, minute as u32, 0)?;
            // A time a daylight-saving change skips doesn't exist that day: it comes the next day.
            if let Some(candidate) = Local.from_local_datetime(&time).earliest() {
                if candidate > after {
                    return Some(candidate.with_timezone(&Utc));
                }
            }
            date = date.succ_opt()?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConflictRule, JobSetup, SyncMode};

    fn job(trigger: Trigger) -> Job {
        Job::new(JobSetup {
            name: "t".into(),
            source: "/a".into(),
            destination: "/b".into(),
            mode: SyncMode::Backup,
            conflict: ConflictRule::Ask,
            trigger,
            excludes: vec![],
            safe_delete: true,
            full_automation: false,
        })
    }

    #[test]
    fn interval_counts_from_creation() {
        let j = job(Trigger::Interval { minutes: 30 });
        assert!(!Schedule::is_due(&j, Utc::now()));
        assert!(Schedule::is_due(&j, Utc::now() + Duration::minutes(31)));
    }

    #[test]
    fn daily_is_within_a_day() {
        let j = job(Trigger::Daily { hour: 3, minute: 15 });
        let next = Schedule::next_run(&j).unwrap();
        assert!(next > Utc::now() && next <= Utc::now() + Duration::hours(25));
    }

    #[test]
    fn manual_never_runs_by_itself() {
        assert!(Schedule::next_run(&job(Trigger::Manual)).is_none());
    }
}
