//! Gli step dell'installer.
//!
//! Ogni step vive in un proprio file: le fasi successive aggiungono un modulo
//! qui senza toccare il motore né gli altri step. In Fase 0 esiste solo
//! [`noop::NoopStep`], usato per testare il motore end-to-end.

use tracing::{info, warn};

use crate::system_ops::SystemOps;

/// Purga pacchetti in un `undo`, **recuperando `dpkg`** se è in stato
/// inconsistente. Best-effort: non fallisce mai, logga cosa resta.
///
/// # Perché serve (A-RT-2, trovato in campo)
///
/// Il rollback arriva sempre *dopo* un fallimento, e quel fallimento può aver
/// lasciato `dpkg` a metà. In quello stato apt si rifiuta di operare
/// (`E: Unmet dependencies. Try 'apt --fix-broken install'`) e **ogni** purge
/// del rollback fallisce: i pacchetti che avevamo installato restano lì e il
/// sistema non torna pulito — la promessa chirurgica violata proprio nello
/// scenario in cui serve di più. Sulla VM di prova restarono 24 pacchetti.
///
/// Perciò: `apt-get install -f` prima del purge (no-op innocuo se `dpkg` è già
/// sano) e, se il purge fallisce lo stesso, un secondo tentativo dopo un
/// `dpkg --configure -a`. Se nemmeno così funziona si prosegue — l'undo è
/// best-effort (invariante 3) — ma si elenca **esattamente** cosa è rimasto,
/// perché è materiale che l'utente dovrà rimuovere a mano.
///
/// Vive qui e non in `SystemOps` perché è **politica di rollback**, non un
/// comando di sistema: il confine resta una mappatura 1:1 sui comandi, e i
/// test possono asserire la sequenza esatta.
pub fn purge_with_dpkg_recovery(ops: &dyn SystemOps, step: &str, pkgs: &[&str]) {
    if pkgs.is_empty() {
        return;
    }

    // Recovery preventivo: apt non opera su un dpkg rotto.
    if let Err(e) = ops.apt_fix_broken() {
        warn!(step, error = %e, "undo: fix-broken preventivo fallito, tento comunque il purge");
    }

    let Err(first) = ops.apt_purge(pkgs) else {
        return;
    };
    warn!(step, error = %first, "undo: purge fallito, tento il recovery di dpkg e riprovo");

    // `dpkg --configure -a` copre i pacchetti scompattati ma non configurati,
    // dove `apt-get install -f` da solo non basta.
    if let Err(e) = ops.dpkg_configure_all() {
        warn!(step, error = %e, "undo: dpkg --configure -a fallito");
    }
    if let Err(e) = ops.apt_fix_broken() {
        warn!(step, error = %e, "undo: fix-broken di recovery fallito");
    }

    match ops.apt_purge(pkgs) {
        Ok(()) => info!(step, "undo: purge riuscito dopo il recovery di dpkg"),
        Err(second) => warn!(
            step,
            error = %second,
            residui = ?pkgs,
            "undo: purge fallito anche dopo il recovery di dpkg, proseguo (best-effort). \
             Questi pacchetti restano installati: rimuovili a mano dopo aver sistemato dpkg \
             (`sudo apt-get install -f`)"
        ),
    }
}

pub mod apt_packages;
pub mod clone_odoo_repo;
pub mod create_database;
pub mod create_db_role;
pub mod create_odoo_user;
pub mod create_virtualenv;
pub mod generate_config;
pub mod initialize_odoo_database;
pub mod install_python_requirements;
pub mod install_wkhtmltopdf;
pub mod nginx_enable_site;
pub mod nginx_firewall;
pub mod nginx_install;
pub mod nginx_reload;
pub mod nginx_write_config;
pub mod noop;
pub mod patch_bashrc;
pub mod prepare_opt_root;
pub mod setup_log_dir;
pub mod setup_postgres;
pub mod setup_systemd;
pub mod write_control_script;
