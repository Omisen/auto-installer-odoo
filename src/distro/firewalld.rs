//! Il firewall della famiglia Fedora: `firewalld`.
//!
//! # Il dettaglio fortunato, e quello che costa
//!
//! **Fortunato**: il token della regola è lo stesso di `ufw`. `firewall-cmd
//! --add-port=80/tcp` accetta la stessa stringa `"80/tcp"` che accetta `ufw
//! allow`, e `--list-ports` la restituisce nella stessa forma. Per questo lo
//! step `nginx-firewall` — cioè il pattern delta, cioè la protezione — non
//! cambia di una riga quando cambia lo strumento sotto.
//!
//! **Costa**: firewalld distingue la configurazione **runtime** da quella
//! **permanente**. Una regola aggiunta senza `--permanent` sparisce al riavvio;
//! una aggiunta solo con `--permanent` non ha effetto finché non si ricarica.
//! Servono entrambe, ed è la ragione per cui questo è un trait e non un gruppo
//! di costanti: un modello che `ufw` non ha non si esprime cambiando una stringa.

use super::Firewall;
use crate::error::StepError;
use crate::system_ops::{capture_command, run_command};

/// `firewalld`, il firewall di Fedora.
#[derive(Debug, Default)]
pub struct Firewalld;

impl Firewall for Firewalld {
    fn name(&self) -> &'static str {
        "firewalld"
    }

    fn available(&self) -> bool {
        std::process::Command::new("firewall-cmd")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// `firewall-cmd --state` esce 0 solo se il demone sta girando.
    ///
    /// Come per `ufw`: installato non basta: se non è attivo non tocchiamo il
    /// firewall, e lo step esce senza mutare nulla.
    fn is_active(&self) -> bool {
        std::process::Command::new("firewall-cmd")
            .arg("--state")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// La regola è già fra le porte aperte **in modo permanente**?
    ///
    /// Si guarda il permanente e non il runtime perché è quello che descrive la
    /// configurazione *voluta* dal cliente: una porta aperta solo a runtime
    /// sparirà da sé al prossimo riavvio, e considerarla «già presente» ci
    /// farebbe rinunciare ad aprirla davvero.
    fn rule_exists(&self, rule: &str) -> Result<bool, StepError> {
        let out = capture_command("firewall-cmd", &["--permanent", "--list-ports"])?;
        Ok(port_in_list(&out, rule))
    }

    /// Apre la porta in **permanente**, poi ricarica per renderla effettiva.
    ///
    /// Due comandi e non uno: `--permanent` scrive la configurazione ma non
    /// tocca il firewall in esecuzione, quindi da solo lascerebbe la porta
    /// chiusa fino al riavvio — nginx sarebbe irraggiungibile e nulla lo
    /// direbbe. Il `--reload` porta il permanente nel runtime, ed è anche ciò
    /// che tiene d'accordo [`Self::rule_exists`] (che legge il permanente) con
    /// quello che il sistema sta davvero facendo.
    ///
    /// L'alternativa — aggiungere due volte, a runtime e in permanente — evita
    /// il reload ma lascia i due stati divergere se uno dei due comandi
    /// fallisce. Meglio una sola sorgente di verità.
    fn allow(&self, rule: &str) -> Result<(), StepError> {
        run_command(
            "firewall-cmd",
            &["--permanent", &format!("--add-port={rule}")],
        )?;
        run_command("firewall-cmd", &["--reload"])
    }

    /// Richiude la porta. Chiamata **solo** sul delta: mai su una regola che il
    /// cliente aveva già.
    fn delete(&self, rule: &str) -> Result<(), StepError> {
        run_command(
            "firewall-cmd",
            &["--permanent", &format!("--remove-port={rule}")],
        )?;
        run_command("firewall-cmd", &["--reload"])
    }
}

/// La porta compare nell'output di `firewall-cmd --list-ports`?
///
/// Confronto per **token**, non per sottostringa — è la stessa protezione di
/// A-V3-7: `"80/tcp"` è contenuto in `"8080/tcp"`, e un confronto largo
/// concluderebbe che la porta 80 è già aperta su una macchina dove non lo è.
/// Da lì la regola non entrerebbe nel delta, il `run` non la aprirebbe, e nginx
/// resterebbe irraggiungibile **senza alcun errore**.
///
/// `--list-ports` stampa le porte su una riga sola, separate da spazi:
///
/// ```text
/// 8080/tcp 443/tcp 53/udp
/// ```
///
/// Pura, perché il caso interessante non è riproducibile su una macchina che non
/// ha firewalld.
pub fn port_in_list(list: &str, rule: &str) -> bool {
    list.split_whitespace().any(|token| token == rule)
}
