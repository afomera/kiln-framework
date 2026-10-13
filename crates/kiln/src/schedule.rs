//! Recurring jobs, declared in code.
//!
//! ```ignore
//! // src/schedule.rs
//! pub fn schedule(s: Schedule) -> Schedule {
//!     s.cron("0 3 * * *", ArchiveOldDrafts {})                        // 03:00 UTC daily
//!      .cron_in("0 9 * * mon", "America/New_York", SendWeeklyDigest {})
//!      .every(SignedDuration::from_mins(15), RefreshFeeds {})
//! }
//!
//! // src/app.rs
//! app.schedule(schedule::schedule)
//! ```
//!
//! Each entry is keyed by its job's name, so a job type is scheduled at most once. Workers
//! sync the entries to `kiln_schedules` when they start, then check for due entries every
//! second. Claiming a due entry and enqueueing its job is one transaction that only one
//! worker can win, so any number of worker processes can run the scheduler. A schedule that
//! was due while nothing was running runs once when workers come back, not once per missed
//! slot. Schedules can be paused, resumed and run on demand from `/kiln/schedules` or
//! `kiln schedules`.

use std::str::FromStr;

use jiff::{Span, ToSpan, Zoned, civil::DateTime, tz::TimeZone};
use serde::Serialize;
use sqlx::Row;

use crate::{
    Db, Error, Result, Timestamp, Value,
    jobs::{JobInfo, insert_job},
};

/// The app's recurring jobs. Built in `src/schedule.rs`, registered with `App::schedule`.
#[derive(Clone, Default)]
pub struct Schedule {
    pub(crate) entries: Vec<Entry>,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub name: &'static str,
    pub when: When,
    pub queue: &'static str,
    pub payload: String,
    pub max_attempts: i64,
    pub priority: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum When {
    Cron { expr: String, tz: String },
    Every { seconds: i64 },
}

impl Schedule {
    /// Runs `job` on a cron schedule in UTC: `"0 3 * * *"` (minute hour day month weekday),
    /// or `@hourly`, `@daily`, `@weekly`, `@monthly`. Panics at boot on a bad expression.
    pub fn cron<J: JobInfo>(self, expr: &str, job: J) -> Self {
        self.cron_in(expr, "UTC", job)
    }

    /// Like [`Schedule::cron`], in a time zone such as `"America/New_York"`.
    pub fn cron_in<J: JobInfo>(self, expr: &str, tz: &str, job: J) -> Self {
        if let Err(e) = Cron::from_str(expr) {
            panic!("schedule for `{}`: {e}", J::NAME);
        }
        if TimeZone::get(tz).is_err() {
            panic!("schedule for `{}`: unknown time zone `{tz}`", J::NAME);
        }
        self.add(J::NAME, When::Cron { expr: expr.to_owned(), tz: tz.to_owned() }, job)
    }

    /// Runs `job` every `interval`, starting one interval after it's first scheduled.
    pub fn every<J: JobInfo>(self, interval: jiff::SignedDuration, job: J) -> Self {
        let seconds = interval.as_secs();
        assert!(seconds >= 1, "schedule for `{}`: the interval must be at least a second", J::NAME);
        self.add(J::NAME, When::Every { seconds }, job)
    }

    fn add<J: JobInfo>(mut self, name: &'static str, when: When, job: J) -> Self {
        if self.entries.iter().any(|e| e.name == name) {
            panic!("`{name}` is scheduled twice; a job type gets one schedule");
        }
        let payload = serde_json::to_string(&job).unwrap_or_else(|e| panic!("job `{name}` doesn't serialize: {e}"));
        self.entries.push(Entry {
            name,
            when,
            queue: J::QUEUE,
            payload,
            max_attempts: J::ATTEMPTS,
            priority: J::PRIORITY,
        });
        self
    }
}

impl When {
    fn kind(&self) -> &'static str {
        match self {
            When::Cron { .. } => "cron",
            When::Every { .. } => "every",
        }
    }

    fn expr(&self) -> String {
        match self {
            When::Cron { expr, .. } => expr.clone(),
            When::Every { seconds } => seconds.to_string(),
        }
    }

    fn tz(&self) -> &str {
        match self {
            When::Cron { tz, .. } => tz,
            When::Every { .. } => "UTC",
        }
    }

    fn from_row(kind: &str, expr: &str, tz: &str) -> Option<Self> {
        match kind {
            "cron" => Some(When::Cron { expr: expr.to_owned(), tz: tz.to_owned() }),
            "every" => expr.parse().ok().map(|seconds| When::Every { seconds }),
            _ => None,
        }
    }

    /// The first run strictly after `after`.
    fn next_after(&self, after: Timestamp) -> Option<Timestamp> {
        match self {
            When::Every { seconds } => Some(after + jiff::SignedDuration::from_secs(*seconds)),
            When::Cron { expr, tz } => {
                let cron = Cron::from_str(expr).ok()?;
                let tz = TimeZone::get(tz).ok()?;
                cron.next_after(&after.as_jiff().to_zoned(tz)).map(|z| Timestamp::from_jiff(z.timestamp()))
            }
        }
    }

    /// "every 15 minutes", "0 3 * * * (UTC)".
    pub(crate) fn describe(&self) -> String {
        match self {
            When::Cron { expr, tz } => format!("{expr} ({tz})"),
            When::Every { seconds } => {
                let (n, unit) = match *seconds {
                    s if s % 86_400 == 0 => (s / 86_400, "day"),
                    s if s % 3600 == 0 => (s / 3600, "hour"),
                    s if s % 60 == 0 => (s / 60, "minute"),
                    s => (s, "second"),
                };
                if n == 1 { format!("every {unit}") } else { format!("every {n} {unit}s") }
            }
        }
    }
}

fn micros(ts: Timestamp) -> i64 {
    ts.as_jiff().as_microsecond()
}

fn from_micros(us: i64) -> Timestamp {
    Timestamp::from_jiff(jiff::Timestamp::from_microsecond(us).unwrap_or_default())
}

/// A schedule's stored state, for the dashboard and `kiln schedules`.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleState {
    pub name: String,
    /// "every 15 minutes", "0 3 * * * (UTC)".
    pub when: String,
    pub queue: String,
    pub next_run_at: Timestamp,
    pub last_run_at: Option<Timestamp>,
    pub paused: bool,
}

impl Db {
    pub(crate) async fn with_schedule_tables(self) -> Result<Self> {
        self.role(crate::db::Role::Queue)
            .execute(
                "CREATE TABLE IF NOT EXISTS \"kiln_schedules\" (\"name\" TEXT PRIMARY KEY, \"kind\" TEXT NOT NULL, \
             \"expr\" TEXT NOT NULL, \"tz\" TEXT NOT NULL, \"queue\" TEXT NOT NULL, \"payload\" TEXT NOT NULL, \
             \"max_attempts\" INTEGER NOT NULL, \"next_run_at\" INTEGER NOT NULL, \"last_run_at\" INTEGER, \
             \"paused\" INTEGER NOT NULL DEFAULT 0)",
            )
            .await?;
        self.ensure_columns("kiln_schedules", &[("priority", "INTEGER NOT NULL DEFAULT 0")]).await?;
        Ok(self)
    }

    /// Makes `kiln_schedules` match the app's declared schedules. Entries whose timing
    /// changed get a new next run; others keep theirs (and their paused state).
    pub(crate) async fn sync_schedules(&self, schedule: &Schedule, now: Timestamp) -> Result<()> {
        for entry in &schedule.entries {
            let next =
                entry.when.next_after(now).ok_or_else(|| Error::Config(format!("`{}` never runs", entry.name)))?;
            self.role(crate::db::Role::Queue).fetch_all(
                "INSERT INTO \"kiln_schedules\" (\"name\", \"kind\", \"expr\", \"tz\", \"queue\", \"payload\", \"max_attempts\", \"next_run_at\", \"priority\") \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (\"name\") DO UPDATE SET \
                 \"next_run_at\" = CASE WHEN \"kiln_schedules\".\"kind\" = excluded.\"kind\" AND \"kiln_schedules\".\"expr\" = excluded.\"expr\" \
                   AND \"kiln_schedules\".\"tz\" = excluded.\"tz\" THEN \"kiln_schedules\".\"next_run_at\" ELSE excluded.\"next_run_at\" END, \
                 \"kind\" = excluded.\"kind\", \"expr\" = excluded.\"expr\", \"tz\" = excluded.\"tz\", \"queue\" = excluded.\"queue\", \
                 \"payload\" = excluded.\"payload\", \"max_attempts\" = excluded.\"max_attempts\", \"priority\" = excluded.\"priority\" RETURNING \"name\"",
                vec![
                    Value::Text(entry.name.to_owned()),
                    Value::Text(entry.when.kind().to_owned()),
                    Value::Text(entry.when.expr()),
                    Value::Text(entry.when.tz().to_owned()),
                    Value::Text(entry.queue.to_owned()),
                    Value::Text(entry.payload.clone()),
                    Value::Integer(entry.max_attempts),
                    Value::Integer(micros(next)),
                    Value::Integer(entry.priority),
                ],
            )
            .await?;
        }
        // Schedules removed from the code stop running.
        let declared: Vec<Value> = schedule.entries.iter().map(|e| Value::Text(e.name.to_owned())).collect();
        let placeholders = vec!["?"; declared.len()].join(", ");
        let sql = if declared.is_empty() {
            "DELETE FROM \"kiln_schedules\" RETURNING \"name\"".to_owned()
        } else {
            format!("DELETE FROM \"kiln_schedules\" WHERE \"name\" NOT IN ({placeholders}) RETURNING \"name\"")
        };
        self.role(crate::db::Role::Queue).fetch_all(&sql, declared).await?;
        Ok(())
    }

    /// Enqueues every schedule due at `now`, each at most once however many slots were
    /// missed. Safe to run from several processes at once. Returns the job names enqueued.
    pub async fn run_due_schedules(&self, now: Timestamp) -> Result<Vec<String>> {
        let due = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT \"name\", \"kind\", \"expr\", \"tz\", \"next_run_at\" FROM \"kiln_schedules\" \
                 WHERE \"paused\" = 0 AND \"next_run_at\" <= ? ORDER BY \"next_run_at\"",
                vec![Value::Integer(micros(now))],
            )
            .await?;
        let mut enqueued = vec![];
        for row in due {
            let (name, kind, expr, tz, slot): (String, String, String, String, i64) =
                (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
            let Some(next) = When::from_row(&kind, &expr, &tz).and_then(|w| w.next_after(now)) else { continue };
            let claimed = self
                .role(crate::db::Role::Queue)
                .transaction(|tx| {
                    let name = name.clone();
                    async move {
                        // Compare-and-set on the slot: one process wins, the rest see no row.
                        let won = tx
                            .role(crate::db::Role::Queue)
                            .fetch_all(
                                "UPDATE \"kiln_schedules\" SET \"next_run_at\" = ?, \"last_run_at\" = ? \
                                 WHERE \"name\" = ? AND \"next_run_at\" = ? AND \"paused\" = 0 \
                                 RETURNING \"queue\", \"payload\", \"max_attempts\", \"priority\"",
                                vec![
                                    Value::Integer(micros(next)),
                                    Value::Integer(micros(now)),
                                    Value::Text(name.clone()),
                                    Value::Integer(slot),
                                ],
                            )
                            .await?;
                        let Some(row) = won.first() else { return Ok(false) };
                        insert_job(
                            &tx,
                            &name,
                            &row.get::<String, _>(0),
                            row.get(1),
                            micros(now),
                            row.get(2),
                            row.get(3),
                        )
                        .await?;
                        Ok(true)
                    }
                })
                .await?;
            if claimed {
                tracing::info!(target: "kiln::schedule", name = %name, next = %next, "scheduled job enqueued");
                enqueued.push(name);
            }
        }
        Ok(enqueued)
    }

    /// Every stored schedule, by next run.
    pub async fn schedules(&self) -> Result<Vec<ScheduleState>> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT \"name\", \"kind\", \"expr\", \"tz\", \"queue\", \"next_run_at\", \"last_run_at\", \"paused\" \
                 FROM \"kiln_schedules\" ORDER BY \"next_run_at\"",
                vec![],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|r| ScheduleState {
                name: r.get(0),
                when: When::from_row(r.get(1), r.get(2), r.get(3)).map(|w| w.describe()).unwrap_or_default(),
                queue: r.get(4),
                next_run_at: from_micros(r.get(5)),
                last_run_at: r.get::<Option<i64>, _>(6).map(from_micros),
                paused: r.get::<i64, _>(7) != 0,
            })
            .collect())
    }

    /// Pauses or resumes a schedule. Resuming skips the slots missed while paused.
    pub async fn set_schedule_paused(&self, name: &str, paused: bool) -> Result<bool> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT \"kind\", \"expr\", \"tz\" FROM \"kiln_schedules\" WHERE \"name\" = ?",
                vec![Value::Text(name.to_owned())],
            )
            .await?;
        let Some(row) = rows.first() else { return Ok(false) };
        let next = When::from_row(row.get(0), row.get(1), row.get(2))
            .and_then(|w| w.next_after(Timestamp::now()))
            .map(micros)
            .unwrap_or(i64::MAX);
        let sql = if paused {
            "UPDATE \"kiln_schedules\" SET \"paused\" = 1 WHERE \"name\" = ? RETURNING \"name\""
        } else {
            "UPDATE \"kiln_schedules\" SET \"paused\" = 0, \"next_run_at\" = ? WHERE \"name\" = ? RETURNING \"name\""
        };
        let binds = if paused {
            vec![Value::Text(name.to_owned())]
        } else {
            vec![Value::Integer(next), Value::Text(name.to_owned())]
        };
        Ok(!self.role(crate::db::Role::Queue).fetch_all(sql, binds).await?.is_empty())
    }

    /// Enqueues a schedule's job now, without moving its next run. Works while paused.
    pub async fn run_schedule_now(&self, name: &str) -> Result<Option<i64>> {
        let rows = self
            .role(crate::db::Role::Queue)
            .fetch_all(
                "SELECT \"queue\", \"payload\", \"max_attempts\", \"priority\" FROM \"kiln_schedules\" WHERE \"name\" = ?",
                vec![Value::Text(name.to_owned())],
            )
            .await?;
        let Some(row) = rows.first() else { return Ok(None) };
        let id = insert_job(
            self,
            name,
            &row.get::<String, _>(0),
            row.get(1),
            micros(Timestamp::now()),
            row.get(2),
            row.get(3),
        )
        .await?;
        Ok(Some(id))
    }
}

/// A 5-field cron expression: minute, hour, day of month, month, day of week.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Cron {
    minutes: Vec<bool>,
    hours: Vec<bool>,
    days: Vec<bool>,
    months: Vec<bool>,
    weekdays: Vec<bool>,
    /// Both day fields restricted: cron matches either one.
    either_day: bool,
}

impl FromStr for Cron {
    type Err = String;

    fn from_str(expr: &str) -> std::result::Result<Self, String> {
        let expr = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            other => other,
        };
        let fields: Vec<&str> = expr.split_whitespace().collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(format!("`{expr}` needs 5 fields: minute hour day month weekday"));
        };
        const MONTHS: &[&str] = &["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
        const DAYS: &[&str] = &["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
        let mut weekdays = field(weekday, 0, 7, DAYS, 0)?;
        if weekdays[7] {
            weekdays[0] = true; // 7 is Sunday too
        }
        weekdays.truncate(7);
        Ok(Cron {
            minutes: field(minute, 0, 59, &[], 0)?,
            hours: field(hour, 0, 23, &[], 0)?,
            days: field(day, 1, 31, &[], 1)?,
            months: field(month, 1, 12, MONTHS, 1)?,
            weekdays,
            either_day: day != "*" && weekday != "*",
        })
    }
}

/// One cron field as a lookup table indexed by value.
fn field(text: &str, min: u32, max: u32, names: &[&str], name_base: u32) -> std::result::Result<Vec<bool>, String> {
    let mut set = vec![false; max as usize + 1];
    let value = |s: &str| -> std::result::Result<u32, String> {
        let lower = s.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == lower) {
            return Ok(i as u32 + name_base);
        }
        let n: u32 = s.parse().map_err(|_| format!("`{s}` isn't a number"))?;
        if n < min || n > max {
            return Err(format!("`{n}` is outside {min}-{max}"));
        }
        Ok(n)
    };
    for part in text.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                (r, s.parse::<u32>().ok().filter(|s| *s > 0).ok_or_else(|| format!("bad step in `{part}`"))?)
            }
            None => (part, 1),
        };
        let (lo, hi) = match range {
            "*" => (min, max),
            r => match r.split_once('-') {
                Some((a, b)) => (value(a)?, value(b)?),
                None if step > 1 => (value(r)?, max),
                None => (value(r)?, value(r)?),
            },
        };
        if lo > hi {
            return Err(format!("`{part}` is an empty range"));
        }
        for n in (lo..=hi).step_by(step as usize) {
            set[n as usize] = true;
        }
    }
    Ok(set)
}

impl Cron {
    fn day_matches(&self, date: jiff::civil::Date) -> bool {
        let dom = self.days[date.day() as usize];
        let dow = self.weekdays[date.weekday().to_sunday_zero_offset() as usize];
        if self.either_day { dom || dow } else { dom && dow }
    }

    /// The first matching minute strictly after `after`, in `after`'s time zone.
    pub(crate) fn next_after(&self, after: &Zoned) -> Option<Zoned> {
        let tz = after.time_zone().clone();
        let start = after.datetime().round(jiff::Unit::Minute).ok()?;
        let mut t: DateTime = if start <= after.datetime() { start.checked_add(1.minute()).ok()? } else { start };
        // Four years covers every valid expression (Feb 29 included).
        let limit = t.checked_add(Span::new().years(4)).ok()?;
        while t < limit {
            if !self.months[t.month() as usize] {
                t = t.date().first_of_month().checked_add(1.month()).ok()?.to_datetime(jiff::civil::Time::midnight());
                continue;
            }
            if !self.day_matches(t.date()) {
                t = t.date().checked_add(1.day()).ok()?.to_datetime(jiff::civil::Time::midnight());
                continue;
            }
            if !self.hours[t.hour() as usize] {
                t = t.with().minute(0).build().ok()?.checked_add(1.hour()).ok()?;
                continue;
            }
            if !self.minutes[t.minute() as usize] {
                t = t.checked_add(1.minute()).ok()?;
                continue;
            }
            let zoned = t.to_zoned(tz.clone()).ok()?;
            // A time skipped by a DST change resolves later; never run in the past.
            if zoned.timestamp() > after.timestamp() {
                return Some(zoned);
            }
            t = t.checked_add(1.minute()).ok()?;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(expr: &str, tz: &str, after: &str) -> String {
        let after: Zoned = format!("{after}[{tz}]").parse().unwrap();
        Cron::from_str(expr).unwrap().next_after(&after).unwrap().strftime("%Y-%m-%d %H:%M %a").to_string()
    }

    #[test]
    fn cron_finds_the_next_slot() {
        assert_eq!(next("0 3 * * *", "UTC", "2026-10-12T10:00"), "2026-10-13 03:00 Tue");
        assert_eq!(next("*/15 * * * *", "UTC", "2026-10-12T10:07"), "2026-10-12 10:15 Mon");
        assert_eq!(next("0 9 * * mon", "UTC", "2026-10-12T09:00"), "2026-10-19 09:00 Mon");
        assert_eq!(next("0 0 29 2 *", "UTC", "2026-03-01T00:00"), "2028-02-29 00:00 Tue");
        assert_eq!(next("@monthly", "UTC", "2026-10-12T00:00"), "2026-11-01 00:00 Sun");
        assert_eq!(next("30 8 * * 1-5", "UTC", "2026-10-16T09:00"), "2026-10-19 08:30 Mon");
        // Both day fields set: either matches (the 1st, or any Friday).
        assert_eq!(next("0 0 1 * fri", "UTC", "2026-10-12T00:00"), "2026-10-16 00:00 Fri");
    }

    #[test]
    fn cron_respects_time_zones() {
        let after: Zoned = "2026-10-12T10:00[America/New_York]".parse().unwrap();
        let at = Cron::from_str("0 9 * * *").unwrap().next_after(&after).unwrap();
        assert_eq!(at.timestamp().to_string(), "2026-10-13T13:00:00Z");
    }

    #[test]
    fn bad_cron_is_explained() {
        assert!(Cron::from_str("0 3 * *").unwrap_err().contains("5 fields"));
        assert!(Cron::from_str("61 * * * *").unwrap_err().contains("outside 0-59"));
        assert!(Cron::from_str("0 0 * * funday").unwrap_err().contains("isn't a number"));
    }

    #[test]
    fn intervals_describe_themselves() {
        assert_eq!(When::Every { seconds: 900 }.describe(), "every 15 minutes");
        assert_eq!(When::Every { seconds: 3600 }.describe(), "every hour");
    }
}
