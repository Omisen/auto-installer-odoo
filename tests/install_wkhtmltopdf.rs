//! Test di [`InstallWkhtmltopdf`] (Fase 4): il cuore è G3 — un checksum errato
//! deve far fallire il run senza installare nulla.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{ops_of, MockConfig, MockDownloader, MockSystemOps, Op};
use odoo_installer::checks::OsInfo;
use odoo_installer::context::Context;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::install_wkhtmltopdf::{map_codename, InstallWkhtmltopdf};
use odoo_installer::system_ops::sha256_hex;

fn ctx(codename: &str) -> Context {
    Context {
        dry_run: false,
        os_info: Some(OsInfo {
            id: "ubuntu".to_string(),
            version: "22.04".to_string(),
            codename: Some(codename.to_string()),
        }),
        ..Default::default()
    }
}

/// SHA-256 reale di `bytes` (scritti in un file di prova).
fn sha_of(bytes: &[u8], dir: &Path) -> String {
    let probe = dir.join("probe.bin");
    std::fs::write(&probe, bytes).expect("write probe");
    sha256_hex(&probe).expect("hash")
}

fn table(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn checksum_mismatch_fails_without_installing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"contenuto .deb fittizio".to_vec();

    let cfg = MockConfig::default(); // wk non installato
    let (mock, log) = MockSystemOps::new(cfg);
    let downloader = MockDownloader::new(bytes, log.clone());

    // Tabella con un hash SBAGLIATO per jammy.
    let checksums = table(&[("jammy", &"00".repeat(32))]);
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        checksums,
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    let result = step.run(&c);

    assert!(result.is_err(), "checksum errato deve far fallire il run (G3)");
    let ops = ops_of(&log);
    assert!(ops.iter().any(|op| matches!(op, Op::Download { .. })), "il download avviene");
    assert!(
        !ops.iter().any(|op| matches!(op, Op::DpkgInstallFile(_))),
        "con checksum errato NON si deve installare il .deb"
    );
}

#[test]
fn checksum_match_installs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"contenuto .deb valido".to_vec();
    let good = sha_of(&bytes, dir.path());

    let cfg = MockConfig::default();
    let (mock, log) = MockSystemOps::new(cfg);
    let downloader = MockDownloader::new(bytes, log.clone());

    let checksums = table(&[("jammy", &good)]);
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        checksums,
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    step.run(&c).expect("run con checksum valido");

    let ops = ops_of(&log);
    assert!(ops.iter().any(|op| matches!(op, Op::DpkgInstallFile(_))), "checksum valido → installa");
    assert!(ops.contains(&Op::AptFixBroken));
    assert_eq!(
        serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
        PreState::CreatedByUs
    );
}

#[test]
fn missing_checksum_refuses_to_install() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = MockConfig::default();
    let (mock, log) = MockSystemOps::new(cfg);
    let downloader = MockDownloader::new(b"x".to_vec(), log.clone());

    // Tabella vuota (come in produzione finché i checksum non sono popolati).
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        BTreeMap::new(),
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    assert!(step.run(&c).is_err(), "senza checksum atteso si rifiuta di installare (G3)");
    assert!(
        !ops_of(&log).iter().any(|op| matches!(op, Op::DpkgInstallFile(_))),
        "nessuna installazione senza checksum verificabile"
    );
}

#[test]
fn preexisting_correct_version_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = MockConfig {
        wk_version: Some("0.12.6.1".to_string()),
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let downloader = MockDownloader::new(b"x".to_vec(), log.clone());
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        BTreeMap::new(),
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    assert_eq!(
        serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
        PreState::Preexisting
    );
    step.run(&c).expect("run");
    step.undo(&c).expect("undo");

    let ops = ops_of(&log);
    assert!(ops.is_empty(), "versione corretta già presente → nessuna azione, trovato: {ops:?}");
}

#[test]
fn codename_mapping() {
    assert_eq!(map_codename(Some("noble")).suffix, "jammy");
    assert!(!map_codename(Some("noble")).fallback);
    assert_eq!(map_codename(Some("bookworm")).suffix, "bookworm");
    assert_eq!(map_codename(Some("bullseye")).suffix, "bullseye");

    let unknown = map_codename(Some("chimera"));
    assert_eq!(unknown.suffix, "jammy");
    assert!(unknown.fallback, "un codename ignoto usa jammy come fallback (con warning)");

    let none = map_codename(None);
    assert_eq!(none.suffix, "jammy");
    assert!(none.fallback);
}
