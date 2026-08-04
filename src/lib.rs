//! Odoo Auto-Installer — motore di installazione reversibile.
//!
//! Questa libreria espone il **motore** dell'installer: il trait [`step::Step`],
//! il pattern [`state::PreState`], il [`context::Context`] e l'orchestratore
//! [`engine::Installer`] (execute + rollback) con persistenza dello stato su
//! disco. In Fase 0 l'unico step è [`steps::noop::NoopStep`]; gli step di
//! sistema reali arrivano nelle fasi successive senza modificare il motore.
//!
//! Le regole invarianti sono descritte in `CLAUDE.md` e nei doc-comment di
//! [`step::Step`] ed [`engine::Installer`].

/// La versione di **questo installer**, dal manifesto di cargo.
///
/// Un posto solo, e tre consumatori: il flag `-V`, la prima riga del log e il
/// manifesto di disinstallazione. Prima non esisteva affatto — `A-V3-16`, trovato
/// installando il `.rpm` della 2.3.0 su una Fedora vera: `--version` è la versione
/// **di Odoo**, e non c'era modo di chiedere al binario la propria. Il difetto
/// vero però non era la scomodità: la versione non compariva **nemmeno nel log**,
/// che questo progetto tiene in vita oltre il rollback proprio perché è il
/// post-mortem. Un log arrivato da una macchina cliente non diceva chi l'avesse
/// scritto, e fra 2.1.0, 2.2.0 e 2.3.0 il comportamento differisce parecchio.
///
/// Non è «un'informazione che c'era e non è stata letta» — la firma solita di
/// questo progetto — ma la sua variante: **un'informazione mai scritta**.
pub const INSTALLER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod checks;
pub mod cli;
pub mod config;
pub mod context;
pub mod distro;
pub mod engine;
pub mod error;
pub mod interrupt;
pub mod lockfile;
pub mod logging;
pub mod packaging;
pub mod progress;
pub mod prompt;
pub mod rollback;
pub mod secret;
pub mod state;
pub mod step;
pub mod steps;
pub mod system_ops;
