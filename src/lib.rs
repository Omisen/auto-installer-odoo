//! Invok — a reversible Odoo installer.
//!
//! this library holds the **engine**: the [`step::Step`] trait, the
//! [`state::PreState`] pattern, the [`context::Context`] and the
//! [`engine::Installer`] orchestrator (execute + rollback) with state persisted
//! to disk. the real system steps live in [`steps`] and plug in without the
//! engine changing.
//!
//! the invariants are described in `CLAUDE.md` and in the doc comments of
//! [`step::Step`] and [`engine::Installer`].

/// version of **this installer**, from the cargo manifest.
///
/// one source, three consumers: the `-V` flag, the log's first line and the
/// uninstall manifest. A-V3-16: `--version` is *Odoo's* version, so before this
/// a log arriving from a customer machine did not say which installer had
/// written it — and 2.1.0, 2.2.0 and 2.3.0 behave quite differently.
pub const INSTALLER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod checks;
pub mod cli;
pub mod config;
pub mod context;
pub mod distro;
pub mod engine;
pub mod error;
pub mod instance;
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
