//! Gli step dell'installer.
//!
//! Ogni step vive in un proprio file: le fasi successive aggiungono un modulo
//! qui senza toccare il motore né gli altri step. In Fase 0 esiste solo
//! [`noop::NoopStep`], usato per testare il motore end-to-end.

pub mod apt_packages;
pub mod create_odoo_user;
pub mod install_wkhtmltopdf;
pub mod noop;
pub mod prepare_opt_root;
pub mod setup_log_dir;
