//! Names and tags go inside a command line, so each is checked before it is
//! sent: one holding a space or a CRLF would split the line, and the rest of it
//! would run as a command of its own.

use std::io;

/// Longest tube, group, owner or entry name tuber accepts, in bytes.
pub(crate) const MAX_NAME_LEN: usize = 200;

/// Check a tube, group, owner or entry name against tuber's tube-name rules:
/// up to 200 bytes of letters, digits and `- + / ; . $ _ ( )`, not starting
/// with `-`.
pub(crate) fn check_name(what: &str, name: &str) -> io::Result<()> {
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
pub(crate) fn check_tag(tag: &str) -> io::Result<()> {
    if tag.is_empty() || tag.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return Err(invalid(format!("invalid tag {tag:?}: a tag can't be empty or hold spaces")));
    }
    Ok(())
}

pub(crate) fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}
