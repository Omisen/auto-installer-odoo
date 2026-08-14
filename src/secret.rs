//! [`Secret`]: a sensitive string that never leaks into the logs.
//!
//! the master password must not appear in logs or in `Debug` output. the real
//! value is reachable only through [`Secret::expose`], used where the secret is
//! genuinely needed — the Odoo config file.

use std::fmt;

/// a sensitive string with a redacted `Debug`.
///
/// deliberately does not implement `Display`.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// wraps a sensitive value.
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// exposes the plaintext. only where the secret is genuinely needed, never
    /// in a log.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `true` when the value is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(****)")
    }
}
