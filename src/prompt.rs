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
                "solo lettere/numeri/._- , nessun '/', '.' o '..'".into(),
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
    println!("── Configurazione installazione Odoo ──");
    println!();

    let mut out = RawConfig::default();

    if cli.version.is_some() {
        info!("Versione Odoo da CLI");
    } else {
        let suggested = env.version.clone().unwrap_or_else(|| "18.0".to_string());
        let start = VERSIONS.iter().position(|v| *v == suggested).unwrap_or(2);
        let choice = Select::new(
            "Versione Odoo",
            VERSIONS.iter().map(|s| s.to_string()).collect(),
        )
        .with_starting_cursor(start)
        .prompt()?;
        out.version = Some(choice);
    }

    if cli.odoo_user.is_some() {
        info!("Utente Odoo da CLI");
    } else {
        let suggested = env.odoo_user.clone().unwrap_or_else(|| "odoo".to_string());
        out.odoo_user = Some(
            Text::new("Utente di sistema Odoo")
                .with_default(&suggested)
                .with_validator(identifier_validator("utente Odoo"))
                .prompt()?,
        );
    }

    if cli.db_name.is_some() {
        info!("Database Odoo da CLI");
    } else {
        let suggested = env.db_name.clone().unwrap_or_else(|| "odoo".to_string());
        out.db_name = Some(
            Text::new("Nome database")
                .with_default(&suggested)
                .with_validator(identifier_validator("nome database"))
                .prompt()?,
        );
    }

    if cli.port.is_some() {
        info!("Porta Odoo da CLI");
    } else {
        let suggested = env.port.clone().unwrap_or_else(|| "8069".to_string());
        out.port = Some(
            Text::new("Porta HTTP")
                .with_default(&suggested)
                .with_validator(port_validator())
                .prompt()?,
        );
    }

    if cli.install_dir.is_some() {
        info!("Install dir da CLI");
    } else {
        let version_for_dir = out
            .version
            .clone()
            .or_else(|| env.version.clone())
            .unwrap_or_else(|| "18.0".to_string());
        let short = version_for_dir.split('.').next().unwrap_or("18");
        let suggested_subdir = format!("odoo{short}");
        let home = config::ODOO_HOME;
        let subdir = Text::new(&format!("Cartella installazione (sotto {home})"))
            .with_default(&suggested_subdir)
            .with_validator(subdir_validator())
            .prompt()?;
        out.install_dir = Some(format!("{home}/{subdir}"));
    }

    if cli.admin_passwd.is_some() {
        info!("Password admin Odoo acquisita da CLI");
    } else {
        let input = Password::new("Password admin Odoo (Invio = valore suggerito)")
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
        let yes = Confirm::new("Configurare Nginx come reverse proxy?")
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
