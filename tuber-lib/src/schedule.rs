//! Recurring schedules: entries tuber puts into a tube at the times a spec
//! names, so an app needs no scheduler process of its own. A fire is an
//! ordinary put; workers reserve the job like any other.
//!
//! An app owns its entries under a name of its choosing, the *owner*, and
//! replaces the whole set with one [`sync_schedules`] at boot. The rules a
//! caller has to keep are in tuber's `docs/scheduling.md`; the ones this
//! module enforces are on [`sync_schedules`].
//!
//! [`sync_schedules`]: crate::client::TuberClient::sync_schedules

use crate::error::Result;
use crate::name::{check_name, invalid};
use crate::parse::{get_str, get_u64, parse_yaml_map};
use std::fmt;

/// The tags a `schedule-put` line may carry: the four a `put` takes, which
/// every fire carries, and `catchup:`, which is the entry's own.
const TAG_PREFIXES: [&str; 5] = ["idp:", "con:", "grp:", "aft:", "catchup:"];

/// One entry of a sync: what each fire puts, and when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleEntry {
    /// The entry's identity within its owner. Keep it stable: a renamed entry
    /// is a removal and an addition, and starts again with no history.
    pub name: String,
    pub tube: String,
    pub pri: u32,
    pub delay: u32,
    pub ttr: u32,
    /// `idp:`, `con:`, `grp:`, `aft:` as on a put, plus `catchup:<secs>`,
    /// verbatim. An `idp:` key skips a fire while the previous fire's job is
    /// still in the tube, but if that job is buried every later fire dedups
    /// against it and queues nothing: watch `buried-streak`.
    pub tags: Vec<String>,
    /// The spec as tuber reads it: `5 * * * *`, `0 2 * * * Australia/Melbourne`,
    /// `@daily`, `every 10m`, `every 1h offset 5m`.
    pub spec: String,
    /// What every fire puts.
    pub body: Vec<u8>,
}

impl ScheduleEntry {
    /// The `schedule-put` line, without its CRLF.
    fn put_line(&self) -> String {
        let mut words = vec![
            "schedule-put".to_string(),
            self.name.clone(),
            self.tube.clone(),
            self.pri.to_string(),
            self.delay.to_string(),
            self.ttr.to_string(),
            self.body.len().to_string(),
        ];
        words.extend(self.tags.iter().cloned());
        words.push(self.spec.trim().to_string());
        words.join(" ")
    }

    /// Refuse, before anything is sent, what tuber would refuse as
    /// `BAD_FORMAT` or misread: names outside tube-name rules, tags without a
    /// known prefix, and specs that would break the line.
    fn validate(&self) -> Result<()> {
        check_name("schedule name", &self.name)?;
        check_name(&format!("tube of schedule {}", self.name), &self.tube)?;
        for tag in &self.tags {
            let known = TAG_PREFIXES
                .iter()
                .any(|p| tag.len() > p.len() && tag.starts_with(p));
            if !known || tag.bytes().any(|b| b <= b' ' || b == 0x7f) {
                return Err(invalid(format!(
                    "schedule {}: tag {tag:?} is not one of idp:, con:, grp:, aft: or catchup: \
                     with a value and no spaces",
                    self.name
                )));
            }
        }
        // A spec token with a colon would be read as a tag, and a control
        // character would end the line or corrupt it.
        let spec = self.spec.trim();
        if spec.is_empty() || spec.contains(':') || spec.chars().any(char::is_control) {
            return Err(invalid(format!(
                "schedule {}: invalid spec {:?}",
                self.name, self.spec
            )));
        }
        Ok(())
    }
}

/// Check a whole sync before any of it is sent: every entry valid, no name
/// twice, and no empty set unless the caller means it.
pub(crate) fn validate_sync(owner: &str, entries: &[ScheduleEntry], allow_empty: bool) -> Result<()> {
    check_name("schedule owner", owner)?;
    if entries.is_empty() && !allow_empty {
        return Err(invalid(format!(
            "refusing to sync no entries for {owner}: that removes all of its schedules"
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        entry.validate()?;
        if !seen.insert(entry.name.as_str()) {
            return Err(invalid(format!("schedule {} appears twice", entry.name)));
        }
    }
    Ok(())
}

/// The `schedule-put` line and body for one entry, framed as `put` frames
/// them, in one buffer so the entry goes out in one write.
pub(crate) fn frame(entry: &ScheduleEntry) -> Vec<u8> {
    let mut buf = entry.put_line().into_bytes();
    buf.extend_from_slice(b"\r\n");
    buf.extend_from_slice(&entry.body);
    buf.extend_from_slice(b"\r\n");
    buf
}

/// What a committed sync did to the owner's entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SyncResult {
    pub added: u64,
    pub changed: u64,
    pub removed: u64,
    pub unchanged: u64,
}

impl SyncResult {
    /// Parse `SYNCED <added> <changed> <removed> <unchanged>`.
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let rest = line.strip_prefix("SYNCED ")?;
        let n: Vec<u64> = rest
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        let [added, changed, removed, unchanged] = n[..] else {
            return None;
        };
        Some(Self { added, changed, removed, unchanged })
    }
}

/// Why a sync didn't take. [`sync_schedules`] returns it as
/// [`Error::ScheduleSync`](crate::Error::ScheduleSync).
///
/// Nothing changed unless [`is_live`](Self::is_live) says otherwise.
///
/// [`sync_schedules`]: crate::client::TuberClient::sync_schedules
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleSyncError {
    pub owner: String,
    /// The entry tuber refused, or `None` when it refused the begin or the
    /// commit.
    pub entry: Option<String>,
    /// Tuber's reply: `BAD_SCHEDULE`, `OUT_OF_MEMORY`, `INTERNAL_ERROR`, …
    pub status: String,
}

impl ScheduleSyncError {
    /// True when the new set is in force despite the error: the commit was
    /// answered `INTERNAL_ERROR`, so the set is live in memory but not on
    /// disk, and a tuber restart before the next successful sync brings back
    /// the previous one.
    pub fn is_live(&self) -> bool {
        self.entry.is_none() && self.status == "INTERNAL_ERROR"
    }
}

impl fmt::Display for ScheduleSyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { owner, entry, status } = self;
        match entry {
            Some(entry) => write!(
                f,
                "tuber refused schedule {entry} for {owner}: {status}; nothing changed, \
                 the previous schedule is still running"
            ),
            None if self.is_live() => write!(
                f,
                "the schedule for {owner} is live but tuber couldn't write it to disk \
                 ({status}); a tuber restart before the next sync brings back the previous one"
            ),
            None if status == "UNKNOWN_COMMAND" => write!(
                f,
                "this server has no recurring schedules ({status}): they need tuber 0.16.0 or later"
            ),
            None => write!(f, "tuber refused the schedule sync for {owner}: {status}; nothing changed"),
        }
    }
}

impl std::error::Error for ScheduleSyncError {}

/// One entry's `stats-schedule`: its definition, next fires, last fire and
/// counters. Times are RFC 3339 in the entry's own zone (UTC for `every`
/// specs), and empty when there is none yet. Counters restart from 0 with
/// the server; `last_fire` is persisted with the entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ScheduleStats {
    pub owner: String,
    pub name: String,
    pub spec: String,
    pub tube: String,
    pub pri: u64,
    pub delay: u64,
    pub ttr: u64,
    /// The `idp:`/`grp:`/`aft:`/`con:` tags each fire carries.
    pub tags: String,
    /// What a missed tick does: `once`, `once within <n>s`, or `never`.
    pub catchup: String,
    /// When this definition took effect: added, or last changed.
    pub synced_at: String,
    pub next_fire: String,
    pub next_fire_2: String,
    pub next_fire_3: String,
    pub last_fire: String,
    /// The reply a client would have got from the latest fire:
    /// `INSERTED <id>`, `INSERTED <id> <STATE>` for a dedup, or the refusal.
    pub last_result: String,
    /// Fires since server start: the sum of the seven counters below.
    pub total_fires: u64,
    pub total_inserted: u64,
    pub total_deduped_ready: u64,
    pub total_deduped_reserved: u64,
    pub total_deduped_delayed: u64,
    pub total_deduped_buried: u64,
    /// Dedups against a cooldown tombstone (`idp:<key>:<ttl>`, job deleted).
    pub total_deduped_deleted: u64,
    /// Fires the put path refused: `DRAINING`, `OUT_OF_MEMORY`,
    /// `OUT_OF_STORAGE`, `INTERNAL_ERROR`.
    pub total_rejected: u64,
    /// Consecutive fires that deduped against a buried job. Non-zero means
    /// the entry is queueing nothing until that job is kicked.
    pub buried_streak: u64,
}

impl ScheduleStats {
    pub fn from_yaml(yaml: &str) -> Self {
        let m = parse_yaml_map(yaml);
        Self {
            owner: get_str(&m, "owner"),
            name: get_str(&m, "name"),
            spec: get_str(&m, "spec"),
            tube: get_str(&m, "tube"),
            pri: get_u64(&m, "pri"),
            delay: get_u64(&m, "delay"),
            ttr: get_u64(&m, "ttr"),
            tags: get_str(&m, "tags"),
            catchup: get_str(&m, "catchup"),
            synced_at: get_str(&m, "synced-at"),
            next_fire: get_str(&m, "next-fire"),
            next_fire_2: get_str(&m, "next-fire-2"),
            next_fire_3: get_str(&m, "next-fire-3"),
            last_fire: get_str(&m, "last-fire"),
            last_result: get_str(&m, "last-result"),
            total_fires: get_u64(&m, "total-fires"),
            total_inserted: get_u64(&m, "total-inserted"),
            total_deduped_ready: get_u64(&m, "total-deduped-ready"),
            total_deduped_reserved: get_u64(&m, "total-deduped-reserved"),
            total_deduped_delayed: get_u64(&m, "total-deduped-delayed"),
            total_deduped_buried: get_u64(&m, "total-deduped-buried"),
            total_deduped_deleted: get_u64(&m, "total-deduped-deleted"),
            total_rejected: get_u64(&m, "total-rejected"),
            buried_streak: get_u64(&m, "buried-streak"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, spec: &str) -> ScheduleEntry {
        ScheduleEntry {
            name: name.into(),
            tube: "splat.maintenance".into(),
            pri: 1024,
            delay: 0,
            ttr: 120,
            tags: vec!["idp:rollup".into(), "con:maintenance".into()],
            spec: spec.into(),
            body: br#"{"class":"HistogramRollupJob","args":[]}"#.to_vec(),
        }
    }

    #[test]
    fn frames_like_the_protocol_example() {
        let framed = frame(&entry("rollup", "5 * * * *"));
        assert_eq!(
            String::from_utf8(framed).unwrap(),
            "schedule-put rollup splat.maintenance 1024 0 120 40 idp:rollup con:maintenance 5 * * * *\r\n\
             {\"class\":\"HistogramRollupJob\",\"args\":[]}\r\n"
        );
    }

    #[test]
    fn accepts_the_spec_forms_tuber_documents() {
        for spec in [
            "5 * * * *",
            "0 2 * * * Australia/Melbourne",
            "CRON_TZ=Australia/Melbourne 0 2 * * *",
            "@daily Europe/London",
            "every 60s",
            "every 1h offset 5m",
        ] {
            assert!(entry("a", spec).validate().is_ok(), "{spec} refused");
        }
    }

    #[test]
    fn refuses_what_would_break_the_line() {
        let cases = [
            ("bad name", entry("a b", "@daily")),
            ("leading dash", entry("-a", "@daily")),
            ("empty spec", entry("a", "  ")),
            ("CRLF in spec", entry("a", "@daily\r\nflush-tube x")),
            ("colon in spec", entry("a", "0 2 * * * x:y")),
            ("unknown tag", ScheduleEntry { tags: vec!["pri:1".into()], ..entry("a", "@daily") }),
            ("bare tag", ScheduleEntry { tags: vec!["idp:".into()], ..entry("a", "@daily") }),
            ("space in tag", ScheduleEntry { tags: vec!["con:a b".into()], ..entry("a", "@daily") }),
            ("bad tube", ScheduleEntry { tube: "".into(), ..entry("a", "@daily") }),
        ];
        for (what, e) in cases {
            let err = e.validate().expect_err(what);
            assert!(matches!(err, crate::Error::InvalidInput(_)), "{what}: {err:?}");
        }
    }

    #[test]
    fn an_empty_set_needs_allow_empty() {
        let err = validate_sync("splat", &[], false).unwrap_err();
        assert!(err.to_string().contains("removes all of its schedules"), "{err}");
        assert!(validate_sync("splat", &[], true).is_ok());
    }

    #[test]
    fn a_name_twice_is_refused_before_sending() {
        let set = [entry("a", "@daily"), entry("a", "@hourly")];
        let err = validate_sync("splat", &set, false).unwrap_err();
        assert!(err.to_string().contains("appears twice"), "{err}");
    }

    #[test]
    fn parses_synced() {
        assert_eq!(
            SyncResult::parse("SYNCED 2 1 3 4"),
            Some(SyncResult { added: 2, changed: 1, removed: 3, unchanged: 4 })
        );
        assert_eq!(SyncResult::parse("SYNCED 2 1 3"), None);
        assert_eq!(SyncResult::parse("ABORTED"), None);
    }

    #[test]
    fn only_a_commit_internal_error_is_live() {
        let err = |entry: Option<&str>, status: &str| ScheduleSyncError {
            owner: "splat".into(),
            entry: entry.map(String::from),
            status: status.into(),
        };
        assert!(err(None, "INTERNAL_ERROR").is_live());
        assert!(err(None, "INTERNAL_ERROR").to_string().contains("is live"));
        assert!(!err(None, "OUT_OF_STORAGE").is_live());
        assert!(!err(Some("rollup"), "BAD_SCHEDULE").is_live());
        let refused = err(Some("retention"), "BAD_SCHEDULE").to_string();
        assert!(refused.contains("retention") && refused.contains("BAD_SCHEDULE"), "{refused}");
        assert!(refused.contains("nothing changed"), "{refused}");
    }

    /// Real `stats-schedule` output: values hold colons, dashes and quotes, and
    /// an absent time is `""`.
    #[test]
    fn parses_stats_schedule() {
        let yaml = "---\n\
            owner: \"splat\"\n\
            name: \"retention\"\n\
            spec: \"0 2 * * * Australia/Melbourne\"\n\
            tube: \"splat.maintenance\"\n\
            pri: 1024\n\
            delay: 0\n\
            ttr: 120\n\
            tags: \"idp:retention con:maintenance\"\n\
            catchup: \"once within 300s\"\n\
            synced-at: 2026-10-11T20:30:00+11:00\n\
            next-fire: 2026-10-12T02:00:00+11:00\n\
            next-fire-2: 2026-10-13T02:00:00+11:00\n\
            next-fire-3: 2026-10-14T02:00:00+11:00\n\
            last-fire: \"\"\n\
            last-result: \"INSERTED 47101 BURIED\"\n\
            total-fires: 7\n\
            total-inserted: 4\n\
            total-deduped-ready: 0\n\
            total-deduped-reserved: 0\n\
            total-deduped-delayed: 0\n\
            total-deduped-buried: 3\n\
            total-deduped-deleted: 0\n\
            total-rejected: 0\n\
            buried-streak: 3\n";
        let s = ScheduleStats::from_yaml(yaml);
        assert_eq!(s.spec, "0 2 * * * Australia/Melbourne");
        assert_eq!(s.tags, "idp:retention con:maintenance");
        assert_eq!(s.catchup, "once within 300s");
        assert_eq!(s.next_fire, "2026-10-12T02:00:00+11:00");
        assert_eq!(s.next_fire_3, "2026-10-14T02:00:00+11:00");
        assert_eq!(s.last_fire, "");
        assert_eq!(s.last_result, "INSERTED 47101 BURIED");
        assert_eq!((s.total_fires, s.total_deduped_buried, s.buried_streak), (7, 3, 3));
    }
}
