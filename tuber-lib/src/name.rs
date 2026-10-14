//! Names and tags go inside a command line, so each is checked before it is
//! sent: one holding a space or a CRLF would split the line, and the rest of it
//! would run as a command of its own.

use crate::error::{Error, Result};

/// Longest tube, group, owner or entry name tuber accepts, in bytes.
pub(crate) const MAX_NAME_LEN: usize = 200;

/// Check a tube, group, owner or entry name against tuber's tube-name rules:
/// up to 200 bytes of letters, digits and `- + / ; . $ _ ( )`, not starting
/// with `-`.
pub(crate) fn check_name(what: &str, name: &str) -> Result<()> {
    let valid_byte = |b: u8| b.is_ascii_alphanumeric() || b"-+/;.$_()".contains(&b);
    if name.is_empty()
        || name.len() > MAX_NAME_LEN
        || name.starts_with('-')
        || !name.bytes().all(valid_byte)
    {
        return Err(invalid(format!("invalid {what} {name:?}")));
    }
    Ok(())
}

/// Check a `put` tag. Tags are passed through as given, so tuber is the judge
/// of what they mean; this only refuses what would break the line: an empty
/// tag, whitespace, or a control byte.
pub(crate) fn check_tag(tag: &str) -> Result<()> {
    if tag.is_empty() || tag.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(invalid(format!("invalid tag {tag:?}: a tag can't be empty or hold spaces")));
    }
    Ok(())
}

pub(crate) fn invalid(msg: String) -> Error {
    Error::InvalidInput(msg)
}

/// Build the tuber tags a [`put_tagged`] carries, checked as tuber will read
/// them.
///
/// Keys and group names keep to tube-name rules, which have no colon. That
/// rules out the trap in writing them by hand: in `idp:series:123` the last
/// colon starts a TTL, so the key is `series`, not `series:123`.
///
/// [`put_tagged`]: crate::TuberClient::put_tagged
pub mod tag {
    use super::check_name;
    use crate::error::{Error, Result};

    /// `idp:<key>`: drop the put if a live job with this key is in the tube.
    pub fn idp(key: &str) -> Result<String> {
        check_name("idempotency key", key)?;
        Ok(format!("idp:{key}"))
    }

    /// `idp:<key>:<ttl>`: as [`idp`], and keep the key for `ttl` seconds
    /// after the job is deleted, so a put that soon after is dropped too.
    pub fn idp_for(key: &str, ttl: u32) -> Result<String> {
        check_name("idempotency key", key)?;
        Ok(format!("idp:{key}:{ttl}"))
    }

    /// `grp:<name>`: add the job to a group, for an [`after`] job to wait on.
    pub fn group(name: &str) -> Result<String> {
        check_name("group name", name)?;
        Ok(format!("grp:{name}"))
    }

    /// `aft:<group>`: hold the job until the group's jobs are done. Tuber
    /// checks the group once, when the job is put or its delay is up, so put
    /// the group's jobs first.
    pub fn after(group: &str) -> Result<String> {
        check_name("group name", group)?;
        Ok(format!("aft:{group}"))
    }

    /// `con:<key>`: at most one job with this key reserved at a time.
    pub fn concurrency(key: &str) -> Result<String> {
        check_name("concurrency key", key)?;
        Ok(format!("con:{key}"))
    }

    /// `con:<key>:<limit>`: at most `limit` jobs with this key reserved at a
    /// time. Jobs that declare different limits get the smallest among those
    /// ready or reserved.
    pub fn concurrency_limit(key: &str, limit: u32) -> Result<String> {
        check_name("concurrency key", key)?;
        if limit == 0 {
            return Err(Error::InvalidInput(format!("concurrency limit for {key} must be at least 1")));
        }
        Ok(format!("con:{key}:{limit}"))
    }
}

#[cfg(test)]
mod tests {
    use crate::tag;

    #[test]
    fn tags_are_built_as_tuber_reads_them() {
        assert_eq!(tag::idp("welcome-42").unwrap(), "idp:welcome-42");
        assert_eq!(tag::idp_for("welcome-42", 3600).unwrap(), "idp:welcome-42:3600");
        assert_eq!(tag::group("batch-1").unwrap(), "grp:batch-1");
        assert_eq!(tag::after("batch-1").unwrap(), "aft:batch-1");
        assert_eq!(tag::concurrency("user-42").unwrap(), "con:user-42");
        assert_eq!(tag::concurrency_limit("user-42", 3).unwrap(), "con:user-42:3");
    }

    /// A colon in a key would be read as the TTL or limit separator, so
    /// `idp:series:123` is key `series` with a 123s TTL. Keys keep to
    /// tube-name rules, which have no colon.
    #[test]
    fn a_key_tuber_would_misread_is_refused() {
        assert!(tag::idp("series:123").is_err());
        assert!(tag::group("a b").is_err());
        assert!(tag::concurrency("").is_err());
        assert!(tag::concurrency_limit("user-42", 0).is_err());
    }
}
