//! Prompt interattivi minimali (via `std::io`).
//!
//! Confine UI: la logica di risoluzione ([`crate::config`]) non dipende da
//! questo modulo. Qui si replica il comportamento del Bash (`collect_inputs`):
//! si prompta **solo** per i campi non passati da CLI, usando come suggerimento
//! il valore da `.env` o il default. I valori raccolti tornano in un
//! [`RawConfig`] che si sovrappone all'`.env` nella cascata.
//!
//! La UI ricca (`inquire`/`indicatif`) è la Fase 11; qui basta un prompt
//! testuale. Nota: l'input della password non è nascosto in questa fase (lo
//! sarà con `inquire` in Fase 11); resta comunque fuori dai log.

use std::io::{self, IsTerminal, Write};

use tracing::info;

use crate::config::{self, RawConfig};

/// `true` se sia stdin sia stdout sono TTY (equivale a `[[ -t 0 && -t 1 ]]`).
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Legge una riga da stdin, senza il newline finale.
fn read_line() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().read_line(&mut buf)?;
    Ok(buf.trim_end_matches(['\n', '\r']).to_string())
}

/// Prompt con default e validatore: Invio conferma il suggerimento; un input
/// non valido viene rifiutato e richiesto di nuovo.
fn prompt_validated<F>(
    label: &str,
    suggested: &str,
    hint: Option<&str>,
    validate: F,
) -> io::Result<String>
where
    F: Fn(&str) -> Result<String, config::ConfigError>,
{
    let suffix = hint.map(|h| format!(" {h}")).unwrap_or_default();
    loop {
        print!("{label}{suffix} [{suggested}]: ");
        io::stdout().flush()?;
        let input = read_line()?;

        if input.is_empty() {
            // Il suggerimento è già un valore valido (env o default).
            return Ok(suggested.to_string());
        }
        match validate(&input) {
            Ok(value) => return Ok(value),
            Err(e) => println!("  Valore non valido: {e}. Riprova."),
        }
    }
}

/// Raccoglie interattivamente i valori per i campi non passati da CLI.
///
/// `cli` serve a sapere quali campi sono già stati forniti (da saltare); `env`
/// fornisce i suggerimenti. Non prompta `db_user` (segue `odoo_user`) né
/// `with_nginx` (solo flag CLI), coerentemente col Bash.
pub fn collect(cli: &RawConfig, env: &RawConfig) -> io::Result<RawConfig> {
    println!();
    println!("Configurazione installazione Odoo");
    println!("Premi Invio per confermare il valore suggerito oppure inseriscine uno diverso.");
    println!();

    let mut out = RawConfig::default();

    // Versione.
    if cli.version.is_some() {
        info!("Versione Odoo da CLI");
    } else {
        let suggested = env.version.clone().unwrap_or_else(|| "18.0".to_string());
        out.version = Some(prompt_validated(
            "Versione Odoo",
            &suggested,
            Some("(16.0/17.0/18.0/19.0)"),
            |v| config::normalize_version(v).map(|(full, _)| full),
        )?);
    }

    // Utente OS.
    if cli.odoo_user.is_some() {
        info!("Utente Odoo da CLI");
    } else {
        let suggested = env.odoo_user.clone().unwrap_or_else(|| "odoo".to_string());
        out.odoo_user = Some(prompt_validated("Utente Odoo", &suggested, None, |v| {
            config::validate_identifier(v, "utente Odoo")
        })?);
    }

    // Nome DB.
    if cli.db_name.is_some() {
        info!("Database Odoo da CLI");
    } else {
        let suggested = env.db_name.clone().unwrap_or_else(|| "odoo".to_string());
        out.db_name = Some(prompt_validated("Database Odoo", &suggested, None, |v| {
            config::validate_identifier(v, "nome database")
        })?);
    }

    // Porta.
    if cli.port.is_some() {
        info!("Porta Odoo da CLI");
    } else {
        let suggested = env.port.clone().unwrap_or_else(|| "8069".to_string());
        out.port = Some(prompt_validated("Porta Odoo", &suggested, None, |v| {
            config::validate_port(v).map(|p| p.to_string())
        })?);
    }

    // Install dir: si prompta la sola sottocartella sotto ODOO_HOME.
    if cli.install_dir.is_some() {
        info!("Install dir da CLI");
    } else {
        // Suggerimento derivato dalla versione già scelta (out/env/default).
        let version_for_dir = out
            .version
            .clone()
            .or_else(|| env.version.clone())
            .unwrap_or_else(|| "18.0".to_string());
        let short = version_for_dir
            .split('.')
            .next()
            .unwrap_or("18")
            .to_string();
        let suggested_subdir = format!("odoo{short}");
        let home = config::ODOO_HOME;
        let subdir = prompt_validated(
            &format!("Cartella installazione (sotto {home})"),
            &suggested_subdir,
            None,
            validate_subdir,
        )?;
        out.install_dir = Some(format!("{home}/{subdir}"));
    }

    // Password admin (input non nascosto in questa fase).
    if cli.admin_passwd.is_some() {
        info!("Password admin Odoo acquisita da CLI");
    } else {
        let suggested = env
            .admin_passwd
            .clone()
            .unwrap_or_else(|| "admin".to_string());
        print!("Password admin Odoo [Invio per il valore suggerito]: ");
        io::stdout().flush()?;
        let input = read_line()?;
        out.admin_passwd = Some(if input.is_empty() { suggested } else { input });
    }

    Ok(out)
}

/// Valida una sottocartella install (nessun `/`, `.` o `..`, solo identifier).
fn validate_subdir(value: &str) -> Result<String, config::ConfigError> {
    let ok = !value.is_empty()
        && !value.starts_with('/')
        && !value.ends_with('/')
        && value.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        });
    if ok {
        Ok(value.to_string())
    } else {
        Err(config::ConfigError::InvalidIdentifier {
            field: "cartella installazione",
            value: value.to_string(),
        })
    }
}

/// Chiede una conferma y/N (default N). Ritorna `true` solo su risposta
/// affermativa esplicita.
pub fn confirm(question: &str) -> io::Result<bool> {
    print!("{question} [y/N]: ");
    io::stdout().flush()?;
    let answer = read_line()?.trim().to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}
