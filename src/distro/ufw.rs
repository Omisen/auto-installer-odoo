//! Il firewall della famiglia Debian: `ufw`.

use super::Firewall;
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// `ufw`, il firewall di Debian/Ubuntu.
#[derive(Debug, Default)]
pub struct Ufw;

impl Firewall for Ufw {
    fn available(&self) -> bool {
        std::process::Command::new("ufw")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn is_active(&self) -> bool {
        capture_command("ufw", &["status"])
            .map(|s| s.contains("Status: active"))
            .unwrap_or(false)
    }

    fn rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let status = capture_command("ufw", &["status"])?;
        Ok(rule_in_status(&status, rule))
    }

    fn allow(&self, rule: &str) -> Result<(), StepError> {
        run_command("ufw", &["allow", rule])
    }

    fn delete(&self, rule: &str) -> Result<(), StepError> {
        run_command("ufw", &["delete", "allow", rule])
    }
}

/// La regola compare in `ufw status`? Confronto per **token**, non per
/// sottostringa (A-V3-7).
///
/// # Il difetto che chiude
///
/// `status.contains("80/tcp")` risponde `true` su una macchina che ha soltanto
/// una regola `8080/tcp` — un'altra app web, un reverse proxy, un runner. Da lì:
/// la regola per la porta 80 non entra nel delta, il `run` non la apre, e nginx
/// viene configurato e ricaricato correttamente ma resta **irraggiungibile
/// dall'esterno**. Nel report non c'è niente di anomalo da leggere.
///
/// # Come si legge `ufw status`
///
/// ```text
/// Status: active
///
/// To                         Action      From
/// --                         ------      ----
/// 80/tcp                     ALLOW       Anywhere
/// 8080/tcp                   ALLOW       Anywhere
/// 80/tcp (v6)                ALLOW       Anywhere (v6)
/// ```
///
/// La regola è il **primo token** della riga (colonna `To`). Il suffisso
/// `(v6)` è un token a parte, quindi la variante IPv6 della stessa porta
/// combacia — ed è giusto: è la stessa regola.
///
/// # Cosa non distingue, dichiarato
///
/// Un profilo applicativo (`Nginx Full`) ha uno spazio nella colonna `To` e non
/// verrà riconosciuto come `80/tcp`, anche se apre quella porta. La conseguenza
/// è che aggiungeremmo `80/tcp` al delta e l'undo la rimuoverebbe: rimuoviamo
/// solo ciò che abbiamo aggiunto noi, quindi la promessa chirurgica regge.
pub fn rule_in_status(status: &str, rule: &str) -> bool {
    /// I token che compaiono in prima colonna senza essere regole.
    const INTESTAZIONI: [&str; 3] = ["To", "--", "Status:"];

    status
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|token| !INTESTAZIONI.contains(token))
        .any(|token| token == rule)
}
