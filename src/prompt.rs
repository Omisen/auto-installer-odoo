//! interactive input with `inquire`. the cascade logic stays in
//! [`crate::config`]; only the *asking* lives here, and the UI never enters a
//! step.
//!
//! only fields **not** passed on the CLI are prompted for, defaulting to the
//! `.env` value or the final default. without a TTY `main` skips this module
//! entirely, so `inquire` never blocks on a terminal that is not there.

use std::io::IsTerminal;

use anyhow::Result;
use inquire::validator::Validation;
use inquire::{Confirm, CustomUserError, Password, PasswordDisplayMode, Select, Text};
use tracing::info;

use crate::config::{self, RawConfig};

const VERSIONS: [&str; 4] = ["16.0", "17.0", "18.0", "19.0"];

/// `true` when both stdin and stdout are TTYs.
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// an `inquire` validator built from a [`crate::config`] one.
fn identifier_validator(
    field: &'static str,
) -> impl Fn(&str) -> Result<Validation, CustomUserError> + Clone {
    move |input: &str| match config::validate_identifier(input, field) {
        Ok(_) => Ok(Validation::Valid),
        Err(e) => Ok(Validation::Invalid(e.to_string().into())),
    }
}

/// the instance-name validator: **the same** rule as everywhere else.
///
/// `crate::instance::validate_instance` and nothing beside it — a second rule
/// living in the prompt would accept here what the cascade refuses three lines
/// later, which is the divergence this project keeps paying for. Empty is a
/// real answer, and means the historical instance.
fn instance_validator() -> impl Fn(&str) -> Result<Validation, CustomUserError> + Clone {
    move |input: &str| {
        if input.is_empty() {
            return Ok(Validation::Valid);
        }
        match crate::instance::validate_instance(input) {
            Ok(_) => Ok(Validation::Valid),
            Err(e) => Ok(Validation::Invalid(e.to_string().into())),
        }
    }
}

/// which instance the prompts below should assume, given what is known so far.
///
/// the **same order as the cascade** in [`crate::config`] — CLI, then what was
/// just answered, then the `.env` — and that is not a nicety: a suggestion
/// resolved differently from the value the cascade will pick would invite
/// somebody to accept a name the installer then ignores.
pub fn instance_in_effect<'a>(
    cli: Option<&'a str>,
    answered: Option<&'a str>,
    env: Option<&'a str>,
) -> Option<&'a str> {
    cli.or(answered).or(env).filter(|s: &&str| !s.is_empty())
}

/// what to suggest for a name the instance qualifies: the system user and the
/// database.
///
/// an `.env` value wins, because it was written on purpose. Otherwise the name
/// the installer itself would derive — `odoo` for the historical instance,
/// `odoo-<name>` for a named one.
///
/// this is **why the instance is asked first**, and the reason is not tidiness.
/// The form always fills these fields, even when the answer is the default, and
/// the cascade falls back to the instance-derived name only when nothing was
/// answered. Suggest `odoo` here and a second instance takes the first one's
/// user, database and port — a collision on all three, produced by the very
/// feature meant to make a second instance easy.
///
/// pure, and deliberately so: `collect` needs a terminal and no test reaches
/// it, so whatever decides has to live outside it.
pub fn suggested_qualified(env_value: Option<&str>, instance: Option<&str>) -> String {
    env_value
        .map(str::to_string)
        .unwrap_or_else(|| crate::instance::qualified_name(instance))
}

fn port_validator() -> impl Fn(&str) -> Result<Validation, CustomUserError> + Clone {
    move |input: &str| match config::validate_port(input) {
        Ok(_) => Ok(Validation::Valid),
        Err(e) => Ok(Validation::Invalid(e.to_string().into())),
    }
}

fn subdir_validator() -> impl Fn(&str) -> Result<Validation, CustomUserError> + Clone {
    move |input: &str| {
        if is_valid_subdir(input) {
            Ok(Validation::Valid)
        } else {
            Ok(Validation::Invalid(
                "letters, digits and ._- only; no '/', '.' or '..'".into(),
            ))
        }
    }
}

/// a valid install subdirectory: no `/`, `.` or `..`, identifier chars only.
fn is_valid_subdir(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('/')
        && !value.ends_with('/')
        && value.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
}

/// collects the fields **not** passed on the CLI, suggesting `env` values as
/// defaults.
///
/// never prompts for `db_user`, which follows `odoo_user`.
///
/// # errors
///
/// propagates an `inquire` failure, including the user aborting a prompt.
pub fn collect(cli: &RawConfig, env: &RawConfig) -> Result<RawConfig> {
    println!();
    println!("── Odoo installation settings ──");
    println!();

    let mut out = RawConfig::default();

    // ASKED FIRST, and the order is the feature (A-V6-15).
    //
    // an instance is created by NAMING it, and until now the only two places
    // that could were `--instance` and `ODOO_INSTANCE`: whoever sat at the
    // terminal was never told that a second Odoo was possible, and found out
    // from a refusal after the fact.
    //
    // first, not last, because every name below is qualified by this one — see
    // `suggested_qualified`. Asked at the end, where the flow would naturally
    // put it, the answers already given for user, database and port would
    // outrank the qualified defaults and the second instance would collide with
    // the first on all three.
    if cli.instance.is_some() {
        info!("instance name taken from the CLI");
    } else {
        let suggested = env.instance.clone().unwrap_or_default();
        let mut question = Text::new("Instance name (empty = the historical instance)")
            .with_help_message(
                "a second Odoo beside an existing one needs a name; leave empty for the first",
            )
            .with_validator(instance_validator());
        if !suggested.is_empty() {
            question = question.with_default(&suggested);
        }
        out.instance = Some(question.prompt()?);
    }
    // what the rest of this form is named after. read through `out` as well as
    // the CLI and the `.env`, which is precisely what makes the answer above
    // reach the questions below.
    let instance = instance_in_effect(
        cli.instance.as_deref(),
        out.instance.as_deref(),
        env.instance.as_deref(),
    )
    .map(str::to_string);
    let instance = instance.as_deref();

    if cli.version.is_some() {
        info!("Odoo version taken from the CLI");
    } else {
        let suggested = env.version.clone().unwrap_or_else(|| "18.0".to_string());
        let start = VERSIONS.iter().position(|v| *v == suggested).unwrap_or(2);
        let choice = Select::new(
            "Odoo version",
            VERSIONS.iter().map(|s| s.to_string()).collect(),
        )
        .with_starting_cursor(start)
        .prompt()?;
        out.version = Some(choice);
    }

    if cli.odoo_user.is_some() {
        info!("Odoo user taken from the CLI");
    } else {
        let suggested = suggested_qualified(env.odoo_user.as_deref(), instance);
        out.odoo_user = Some(
            Text::new("Odoo system user")
                .with_default(&suggested)
                .with_validator(identifier_validator("Odoo user"))
                .prompt()?,
        );
    }

    if cli.db_name.is_some() {
        info!("Odoo database taken from the CLI");
    } else {
        let suggested = suggested_qualified(env.db_name.as_deref(), instance);
        out.db_name = Some(
            Text::new("Database name")
                .with_default(&suggested)
                .with_validator(identifier_validator("database name"))
                .prompt()?,
        );
    }

    if cli.port.is_some() {
        info!("Odoo port taken from the CLI");
    } else {
        let suggested = env.port.clone().unwrap_or_else(|| "8069".to_string());
        out.port = Some(
            Text::new("HTTP port")
                .with_default(&suggested)
                .with_validator(port_validator())
                .prompt()?,
        );
    }

    if cli.install_dir.is_some() {
        info!("Install dir taken from the CLI");
    } else {
        let version_for_dir = out
            .version
            .clone()
            .or_else(|| env.version.clone())
            .unwrap_or_else(|| "18.0".to_string());
        let short = version_for_dir.split('.').next().unwrap_or("18");
        // the suggestion follows the instance when there is one: proposing
        // `odoo18` to somebody who named `cliente-x` would invite them to
        // accept a directory named after the wrong thing.
        let suggested_subdir = crate::instance::artifact_base(instance, short);
        let home = config::ODOO_HOME;
        let subdir = Text::new(&format!("Install directory (under {home})"))
            .with_default(&suggested_subdir)
            .with_validator(subdir_validator())
            .prompt()?;
        out.install_dir = Some(format!("{home}/{subdir}"));
    }

    if cli.admin_passwd.is_some() {
        info!("Odoo admin password taken from the CLI");
    } else {
        let input = Password::new("Odoo admin password (Enter = suggested value)")
            .with_display_mode(PasswordDisplayMode::Masked)
            .without_confirmation()
            .prompt()?;
        if !input.is_empty() {
            out.admin_passwd = Some(input);
        } else if let Some(env_pw) = env.admin_passwd.clone() {
            out.admin_passwd = Some(env_pw);
        }
        // empty and no env value → stays None → the cascade defaults it.
    }

    if cli.with_nginx.is_none() {
        let default = env.with_nginx.unwrap_or(false);
        let yes = Confirm::new("Configure nginx as a reverse proxy?")
            .with_default(default)
            .prompt()?;
        out.with_nginx = Some(yes);
    }

    Ok(out)
}

/// asks a yes/no question, defaulting to no.
///
/// # errors
///
/// propagates an `inquire` failure.
pub fn confirm(question: &str) -> Result<bool> {
    Ok(Confirm::new(question).with_default(false).prompt()?)
}
