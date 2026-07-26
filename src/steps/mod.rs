//! Gli step dell'installer.
//!
//! Ogni step vive in un proprio file: le fasi successive aggiungono un modulo
//! qui senza toccare il motore né gli altri step. In Fase 0 esiste solo
//! [`noop::NoopStep`], usato per testare il motore end-to-end.

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
pub mod prepare_opt_root;
pub mod setup_log_dir;
pub mod setup_postgres;
pub mod setup_systemd;
