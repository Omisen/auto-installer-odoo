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

pub mod checks;
pub mod cli;
pub mod config;
pub mod context;
pub mod engine;
pub mod error;
pub mod lockfile;
pub mod logging;
pub mod progress;
pub mod prompt;
pub mod rollback;
pub mod secret;
pub mod state;
pub mod step;
pub mod steps;
pub mod system_ops;
