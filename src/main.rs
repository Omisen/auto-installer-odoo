//! Entry point minimale: dimostra che il motore gira.
//!
//! In Fase 0 costruisce un [`Context`] fittizio, monta una sequenza di
//! [`NoopStep`] e chiama [`Installer::execute`]. Il parsing `.env`/CLI (Fase 1)
//! e la UI interattiva (Fase 11) non sono ancora presenti: qui il `Context` è
//! costruito a mano e l'esecuzione è in `dry_run` per non toccare il sistema.

use anyhow::Result;

use odoo_installer::context::Context;
use odoo_installer::engine::Installer;
use odoo_installer::step::Step;
use odoo_installer::steps::noop::NoopStep;

fn main() -> Result<()> {
    init_tracing();

    // Context fittizio in dry-run: nessuna mutazione, nessuna scrittura di stato.
    let ctx = Context::new("18.0", "odoo", "/opt/odoo", "odoo", /* dry_run */ true);
    tracing::info!(
        odoo_version = %ctx.odoo_version,
        odoo_user = %ctx.odoo_user,
        odoo_home = %ctx.odoo_home.display(),
        db_name = %ctx.db_name,
        dry_run = ctx.dry_run,
        "avvio installer (demo motore)"
    );

    let mut steps: Vec<Box<dyn Step>> = vec![
        Box::new(NoopStep::new("preflight")),
        Box::new(NoopStep::new("system-user")),
        Box::new(NoopStep::new("sources")),
    ];

    let mut installer = Installer::new();
    match installer.execute(&mut steps, &ctx) {
        Ok(()) => {
            tracing::info!("installazione completata");
            Ok(())
        }
        Err(e) => {
            // Il rollback è già stato eseguito dentro `execute`.
            tracing::error!(error = %e, "installazione fallita, rollback eseguito");
            Err(e.into())
        }
    }
}

/// Inizializza `tracing` verso il TTY, con livello controllabile da `RUST_LOG`
/// (default `info`).
fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // `try_init` fallisce solo se un subscriber globale è già installato:
    // ignorarlo è corretto e non è un `unwrap`/`expect` di produzione.
    let _ = fmt().with_env_filter(filter).try_init();
}
