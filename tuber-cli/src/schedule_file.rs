//! The file `tuber-cli schedule-sync` reads: a JSON array of entries, in the
//! option names tuber-gem's `schedules.sync` takes.
//!
//! ```json
//! [
//!   {"name": "rollup", "tube": "splat.maintenance", "cron": "5 * * * *",
//!    "idp": "rollup", "con": "maintenance", "pri": 1024,
//!    "body": {"class": "HistogramRollupJob", "args": []}},
//!   {"name": "retention", "tube": "splat.maintenance",
//!    "cron": "0 2 * * *", "zone": "Australia/Melbourne",
//!    "body": "{\"class\":\"DataRetentionJob\",\"args\":[]}"},
//!   {"name": "monitor", "tube": "splat.checkins", "every": 60, "catchup": "never",
//!    "body": "monitor"}
//! ]
//! ```

use anyhow::{bail, Context};
use serde::Deserialize;
use tuber_lib::schedule::ScheduleEntry;

/// Defaults for what a put line carries, as tuber-gem defaults them, so an
/// entry means the same schedule whichever syncs it. The priority is not
/// `tuber-cli put`'s 0: under 1024 is urgent, and a recurring job left at
/// the default should not jump ahead of everything else.
const DEFAULT_PRI: u32 = 65536;
const DEFAULT_DELAY: u32 = 0;
const DEFAULT_TTR: u32 = 120;

/// One entry as written in the file. Unknown fields are refused: a
/// misspelled `"zone"` would otherwise run the entry in UTC without a word.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileEntry {
    name: String,
    tube: String,
    /// A string is put verbatim; any other JSON value is put as written, less
    /// the whitespace between tokens. Kept raw, since a parsed object would
    /// come back with its keys sorted.
    body: Box<serde_json::value::RawValue>,
    cron: Option<String>,
    zone: Option<String>,
    every: Option<Interval>,
    offset: Option<Interval>,
    spec: Option<String>,
    pri: Option<u32>,
    delay: Option<u32>,
    ttr: Option<u32>,
    idp: Option<String>,
    con: Option<String>,
    grp: Option<String>,
    aft: Option<String>,
    catchup: Option<Catchup>,
}

/// Seconds, or tuber's own `10m` form.
#[derive(Deserialize)]
#[serde(untagged)]
enum Interval {
    Secs(u64),
    Text(String),
}

/// Only `false`: `true` is no answer to how stale a catch-up may be.
struct False;

impl<'de> Deserialize<'de> for False {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match bool::deserialize(d)? {
            false => Ok(False),
            true => Err(serde::de::Error::custom("catchup is false, not true")),
        }
    }
}

impl Interval {
    fn text(&self) -> String {
        match self {
            Self::Secs(n) => format!("{n}s"),
            Self::Text(t) => t.clone(),
        }
    }
}

/// What a tick missed while tuber was down does: `"once"` (the default),
/// `"never"` (or 0, or `false` as the gem has it), or fire it only if at most
/// that many seconds stale.
#[derive(Deserialize)]
#[serde(untagged)]
enum Catchup {
    Never(False),
    Secs(u32),
    Word(String),
}

/// Parse a schedule file into the entries of one sync.
pub fn parse(json: &str) -> anyhow::Result<Vec<ScheduleEntry>> {
    let entries: Vec<FileEntry> =
        serde_json::from_str(json).context("schedule file must be a JSON array of entries")?;
    entries
        .into_iter()
        .enumerate()
        .map(|(i, e)| {
            let name = e.name.clone();
            e.into_entry()
                .with_context(|| format!("schedule entry {} ({name})", i + 1))
        })
        .collect()
}

impl FileEntry {
    fn into_entry(self) -> anyhow::Result<ScheduleEntry> {
        let spec = match (&self.spec, &self.cron, &self.every) {
            (Some(spec), None, None) => spec.clone(),
            (None, Some(cron), None) => match &self.zone {
                Some(zone) => format!("{cron} {zone}"),
                None => cron.clone(),
            },
            (None, None, Some(every)) => match &self.offset {
                Some(offset) => format!("every {} offset {}", every.text(), offset.text()),
                None => format!("every {}", every.text()),
            },
            _ => bail!("give exactly one of spec, cron or every"),
        };
        if self.zone.is_some() && self.cron.is_none() {
            bail!("zone goes with cron");
        }
        if self.offset.is_some() && self.every.is_none() {
            bail!("offset goes with every");
        }

        let mut tags = Vec::new();
        for (prefix, value) in [("idp", &self.idp), ("con", &self.con), ("grp", &self.grp), ("aft", &self.aft)] {
            if let Some(v) = value {
                tags.push(format!("{prefix}:{v}"));
            }
        }
        match &self.catchup {
            None => {}
            Some(Catchup::Word(w)) if w == "once" => {}
            Some(Catchup::Word(w)) if w == "never" => tags.push("catchup:0".into()),
            Some(Catchup::Never(False)) => tags.push("catchup:0".into()),
            Some(Catchup::Secs(n)) => tags.push(format!("catchup:{n}")),
            Some(Catchup::Word(w)) => bail!("catchup is \"once\", \"never\" or seconds, not {w:?}"),
        }

        let raw = self.body.get();
        let body = if raw.starts_with('"') {
            serde_json::from_str::<String>(raw)?.into_bytes()
        } else {
            compact(raw).into_bytes()
        };

        Ok(ScheduleEntry {
            name: self.name,
            tube: self.tube,
            pri: self.pri.unwrap_or(DEFAULT_PRI),
            delay: self.delay.unwrap_or(DEFAULT_DELAY),
            ttr: self.ttr.unwrap_or(DEFAULT_TTR),
            tags,
            spec,
            body,
        })
    }
}

/// `json` without the whitespace between its tokens. `json` is valid JSON, so
/// whitespace outside a string is never significant.
fn compact(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    let (mut in_string, mut escaped) = (false, false);
    for c in json.chars() {
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
        } else if c.is_ascii_whitespace() {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(json: &str) -> anyhow::Result<ScheduleEntry> {
        parse(&format!("[{json}]")).map(|mut v| v.remove(0))
    }

    #[test]
    fn cron_and_zone_make_one_spec() {
        let e = one(r#"{"name":"r","tube":"t","cron":"0 2 * * *","zone":"Australia/Melbourne","body":"x"}"#)
            .unwrap();
        assert_eq!(e.spec, "0 2 * * * Australia/Melbourne");
        // The gem's defaults, so one entry means the same schedule from either.
        assert_eq!((e.pri, e.delay, e.ttr), (65536, 0, 120));
        assert_eq!(e.body, b"x");
        assert!(e.tags.is_empty());
    }

    #[test]
    fn every_takes_seconds_or_tubers_own_form() {
        let e = one(r#"{"name":"m","tube":"t","every":60,"body":"x"}"#).unwrap();
        assert_eq!(e.spec, "every 60s");
        let e = one(r#"{"name":"m","tube":"t","every":"1h","offset":"5m","body":"x"}"#).unwrap();
        assert_eq!(e.spec, "every 1h offset 5m");
    }

    #[test]
    fn tags_and_catchup() {
        let e = one(
            r#"{"name":"r","tube":"t","spec":"@daily","idp":"r","con":"maintenance:2",
                "grp":"g","aft":"a","catchup":300,"body":"x"}"#,
        )
        .unwrap();
        assert_eq!(e.tags, ["idp:r", "con:maintenance:2", "grp:g", "aft:a", "catchup:300"]);
        let never = |c: &str| one(&format!(r#"{{"name":"r","tube":"t","spec":"@daily","catchup":{c},"body":"x"}}"#));
        assert_eq!(never("\"never\"").unwrap().tags, ["catchup:0"]);
        assert_eq!(never("0").unwrap().tags, ["catchup:0"]);
        assert_eq!(never("false").unwrap().tags, ["catchup:0"]);
        assert!(never("\"once\"").unwrap().tags.is_empty());
        assert!(never("\"sometimes\"").is_err());
        assert!(never("true").is_err());
    }

    #[test]
    fn a_json_body_is_put_as_written_less_whitespace() {
        let e = one(
            r#"{"name":"r","tube":"t","spec":"@daily",
                "body": {"class": "RollupJob", "args": ["a b", "\" x"]}}"#,
        )
        .unwrap();
        assert_eq!(String::from_utf8(e.body).unwrap(), r#"{"class":"RollupJob","args":["a b","\" x"]}"#);
    }

    #[test]
    fn a_string_body_is_put_verbatim() {
        let e = one(r#"{"name":"r","tube":"t","spec":"@daily","body":"line 1\nline 2"}"#).unwrap();
        assert_eq!(e.body, b"line 1\nline 2");
    }

    #[test]
    fn refuses_ambiguous_or_misspelled_entries() {
        for json in [
            r#"{"name":"r","tube":"t","body":"x"}"#,
            r#"{"name":"r","tube":"t","cron":"@daily","every":60,"body":"x"}"#,
            r#"{"name":"r","tube":"t","every":60,"zone":"Australia/Melbourne","body":"x"}"#,
            r#"{"name":"r","tube":"t","cron":"0 2 * * *","offset":"5m","body":"x"}"#,
            r#"{"name":"r","tube":"t","cron":"0 2 * * *","zome":"Australia/Melbourne","body":"x"}"#,
            r#"{"name":"r","tube":"t","cron":"0 2 * * *"}"#,
        ] {
            assert!(one(json).is_err(), "accepted {json}");
        }
    }

    #[test]
    fn an_error_names_the_entry() {
        let err = parse(r#"[{"name":"ok","tube":"t","spec":"@daily","body":"x"},
                            {"name":"broken","tube":"t","body":"x"}]"#)
            .unwrap_err();
        assert!(format!("{err:#}").contains("entry 2 (broken)"), "{err:#}");
    }
}
