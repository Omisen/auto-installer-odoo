//! the instance name, and every name derived from it (phase I0).
//!
//! more than one Odoo on one machine is, in the end, a question of **names**:
//! the 25 steps already know how to be a no-op on what somebody else created
//! (that is what `PreState` is for), so what was missing was a way to say *which*
//! installation this one is. that is `--instance`.
//!
//! # two families of names, and why they are not one
//!
//! the artifacts split in two, by what their name looks like **today**:
//!
//! - [`artifact_base`] — unit, config file, vhost, install dir. today they carry
//!   the Odoo **version**: `odoo18`. one per version already, so two instances
//!   of two different versions never collided; two of the same version did.
//! - [`qualified_name`] — system user, PostgreSQL role, database, `odoo` helper.
//!   today they are the bare word `odoo`, shared by construction.
//!
//! for a **named** instance the two collapse into the same string
//! (`odoo-<name>`), and that is exactly why more than one instance becomes
//! possible: every artifact is qualified by who owns it. for the **unnamed**
//! instance — nobody passed `--instance` — each family keeps its historical
//! value, byte for byte. that is the I0 contract: whoever does not ask for an
//! instance must not notice that instances exist.
//!
//! # why the name is validated, and why *here*
//!
//! `A-V6-1`. one name ends up in five different grammars — a systemd unit, a
//! path, a PostgreSQL identifier, an nginx `server_name`, a Unix user — and each
//! rejects a different thing. an unchecked name does not fail once: it fails
//! *somewhere*, and the somewhere is halfway through the sequence, after shared
//! artifacts have already been touched. so the rule is the **intersection** of
//! the five, applied before anything is mutated.
//!
//! the length is the part that is easy to get wrong, and the reason (α) made it
//! sharp: the system user of a named instance is `odoo-<name>`, and a Unix user
//! name stops at 32 (`UT_NAMESIZE`). over that, `useradd` does not always
//! refuse — it may **truncate**, and a truncated user with an untruncated role
//! breaks `peer` authentication silently, which is the worst way this could
//! show up.

use crate::config::ConfigError;

/// how long an instance name may be.
///
/// 26, and not a round number: the system user is `odoo-` + the name, and Unix
/// stops at 32 (`UT_NAMESIZE`). 5 + 26 = 31 leaves one character of margin.
pub const MAX_INSTANCE_LEN: usize = 26;

/// what a named instance's artifacts are prefixed with.
///
/// namespaces them: `odoo-cliente-x` is visibly ours as a user, a unit and a
/// role, and cannot collide with a human account that happens to share the
/// instance name.
pub const INSTANCE_PREFIX: &str = "odoo-";

/// the historical, unqualified name: user, role, database and helper of the
/// unnamed instance.
pub const PLAIN_NAME: &str = "odoo";

/// how the **unnamed** instance is named when one has to be typed: to
/// `rollback --instance`, and in `list`'s output.
///
/// it is therefore a **reserved word**: [`validate_instance`] refuses it, so a
/// real instance can never take it. an instance actually called `default` would
/// make a destructive command's selector ambiguous, and ambiguity there is not
/// something to settle with a precedence rule — it is something to make
/// impossible.
pub const UNNAMED_ID: &str = "default";

/// validates an instance name against the intersection of the five grammars it
/// will end up in: `^[a-z][a-z0-9_-]{0,25}$`.
///
/// lowercase only, because two of the five consumers are case-insensitive in
/// practice (PostgreSQL folds unquoted identifiers, and a path is not folded at
/// all): allowing both would let `Cliente` and `cliente` name the same role and
/// two different directories.
///
/// # errors
///
/// [`ConfigError::InvalidInstance`], naming which rule was broken — the length
/// and the alphabet fail for different reasons and have different fixes.
pub fn validate_instance(value: &str) -> Result<String, ConfigError> {
    let invalid = |reason: &'static str| ConfigError::InvalidInstance {
        value: value.to_string(),
        reason,
    };

    if value.is_empty() {
        return Err(invalid("it is empty"));
    }
    if value == UNNAMED_ID {
        return Err(invalid(
            "'default' is reserved: it names the instance installed without --instance, so a real one may not take it",
        ));
    }
    if value.len() > MAX_INSTANCE_LEN {
        return Err(invalid(
            "it is longer than 26 characters, and the system user derived from it \
             ('odoo-<name>') would not fit the 32 a Unix user name allows",
        ));
    }
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => {
            return Err(invalid(
                "it must start with a lowercase letter (a leading digit is not a valid \
                 systemd unit name, and a leading '-' would be read as an option)",
            ))
        }
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_')) {
        return Err(invalid(
            "it may only contain lowercase letters, digits, '-' and '_'",
        ));
    }
    Ok(value.to_string())
}

/// the base name of the **versioned** artifacts: unit, config file, vhost,
/// install dir.
///
/// unnamed → `odoo18`, exactly what every release so far has written on disk.
/// named → `odoo-cliente-x`, which drops the version because the name is now
/// what tells two installations apart; the version is still recorded in the
/// manifest and in the unit's `Description`.
pub fn artifact_base(instance: Option<&str>, version_short: &str) -> String {
    match instance {
        Some(name) => format!("{INSTANCE_PREFIX}{name}"),
        None => format!("{PLAIN_NAME}{version_short}"),
    }
}

/// the name of the artifacts that are **plain `odoo`** today: system user,
/// PostgreSQL role, database, `odoo` helper command.
///
/// unnamed → `odoo`. named → `odoo-cliente-x`, the same string
/// [`artifact_base`] returns, which is the point: for a named instance every
/// artifact is qualified by the same name, so nothing is shared implicitly.
///
/// this is a **default**, not a decision: the CLI → `.env` cascade can still
/// override the user, the role and the database explicitly, as it always could.
pub fn qualified_name(instance: Option<&str>) -> String {
    match instance {
        Some(name) => format!("{INSTANCE_PREFIX}{name}"),
        None => PLAIN_NAME.to_string(),
    }
}
