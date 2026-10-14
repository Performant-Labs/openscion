//! Limits on names that clients send: repos, refs and request keys.

use crate::error::{Error, Result};

/// Longest accepted name, in bytes.
pub const MAX_NAME_BYTES: usize = 256;

/// Checks a client-supplied name: 1 to [`MAX_NAME_BYTES`] bytes and no
/// control characters, so names are safe to log and to print.
///
/// # Errors
///
/// [`Error::InvalidArgument`] naming `what` if the name breaks a rule.
pub fn check(what: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES {
        return Err(Error::InvalidArgument(format!(
            "{what} must be 1 to {MAX_NAME_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(Error::InvalidArgument(format!(
            "{what} must not contain control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ordinary_names_and_rejects_the_rest() {
        for ok in [
            "main",
            "release/2026.10",
            "bench:abc",
            &"x".repeat(MAX_NAME_BYTES),
        ] {
            assert!(check("ref", ok).is_ok(), "{ok:?}");
        }
        for bad in [
            "",
            "a\nb",
            "\u{1b}[31m",
            "tab\there",
            &"x".repeat(MAX_NAME_BYTES + 1),
        ] {
            assert!(check("ref", bad).is_err(), "{bad:?}");
        }
    }
}
