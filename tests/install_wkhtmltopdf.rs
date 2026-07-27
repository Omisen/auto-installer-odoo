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
use odoo_installer::steps::install_wkhtmltopdf::{
    default_checksums, map_codename, InstallWkhtmltopdf,
};
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

    assert!(
        result.is_err(),
        "checksum errato deve far fallire il run (G3)"
    );
    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::Download { .. })),
        "il download avviene"
    );
    assert!(
        !ops.iter().any(|op| matches!(op, Op::AptInstallDebFile(_))),
        "con checksum errato NON si deve installare il .deb"
    );
}

#[test]
fn checksum_match_installs() {
    // Ramo felice, per **ognuno** dei tre suffissi realmente pubblicati dalla
    // release 0.12.6.1-3: download → hash combacia col pin → install via apt.
    for (codename, suffix) in [
        ("jammy", "jammy"),
        ("noble", "jammy"),
        ("bullseye", "bullseye"),
        ("bookworm", "bookworm"),
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let bytes = format!("contenuto .deb valido per {suffix}").into_bytes();
        let good = sha_of(&bytes, dir.path());

        let cfg = MockConfig::default();
        let (mock, log) = MockSystemOps::new(cfg);
        let downloader = MockDownloader::new(bytes, log.clone());

        let checksums = table(&[(suffix, &good)]);
        let mut step = InstallWkhtmltopdf::with_parts(
            Box::new(mock),
            Box::new(downloader),
            checksums,
            dir.path().to_path_buf(),
        );
        let c = ctx(codename);

        step.snapshot(&c).expect("snapshot");
        step.run(&c).expect("run con checksum valido");

        let ops = ops_of(&log);
        // Il .deb scaricato è quello del suffisso mappato, non del codename.
        assert!(
            ops.iter().any(|op| matches!(op, Op::Download { url, .. }
                if url.ends_with(&format!("wkhtmltox_0.12.6.1-3.{suffix}_amd64.deb")))),
            "{codename} deve scaricare il .deb {suffix}, trovato: {ops:?}"
        );
        assert!(
            ops.iter().any(|op| matches!(op, Op::AptInstallDebFile(_))),
            "checksum valido → installa ({codename})"
        );
        assert_eq!(
            serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
            PreState::CreatedByUs
        );
    }
}

/// A-RT-1 (trovato in campo su Multipass, Ubuntu 22.04 minimale): il `.deb` di
/// wkhtmltopdf dipende da `fontconfig`, `libxrender1`, `xfonts-75dpi`,
/// `xfonts-base`, assenti su una VM pulita. Con `dpkg -i` lo step falliva
/// sempre — e per giunta lasciava dpkg rotto. L'installazione deve passare da
/// un comando che **risolve le dipendenze**.
#[test]
fn install_resolves_dependencies_instead_of_bare_dpkg() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"contenuto .deb valido".to_vec();
    let good = sha_of(&bytes, dir.path());

    let (mock, log) = MockSystemOps::new(MockConfig::default());
    let downloader = MockDownloader::new(bytes, log.clone());
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        table(&[("jammy", &good)]),
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    step.run(&c)
        .expect("su un sistema minimale l'installazione deve riuscire");

    let ops = ops_of(&log);
    assert!(
        ops.iter().any(|op| matches!(op, Op::AptInstallDebFile(p)
            if p.to_string_lossy().ends_with(".deb"))),
        "il .deb va installato con apt, che risolve le dipendenze: {ops:?}"
    );
}

/// Ma un errore vero resta un errore: se l'installazione fallisce davvero (non
/// per dipendenze risolvibili), lo step deve fallire e innescare il rollback.
#[test]
fn a_real_install_failure_still_fails_the_step() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"contenuto .deb valido".to_vec();
    let good = sha_of(&bytes, dir.path());

    let cfg = MockConfig {
        apt_install_deb_fails: true,
        ..Default::default()
    };
    let (mock, log) = MockSystemOps::new(cfg);
    let downloader = MockDownloader::new(bytes, log.clone());
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        table(&[("jammy", &good)]),
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    assert!(
        step.run(&c).is_err(),
        "un fallimento reale dell'installazione non va inghiottito"
    );
    assert_eq!(
        serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
        PreState::Untracked,
        "run fallito → niente da annullare"
    );
}

/// Ramo felice con la **tabella di produzione**: i pin non sono più vuoti,
/// quindi lo step arriva davvero fino al confronto (prima falliva sempre prima,
/// su "checksum non disponibile"). Qui il `.deb` mock è ovviamente diverso da
/// quello vero → il confronto col pin reale deve fallire, senza installare.
#[test]
fn production_pins_reject_a_deb_that_is_not_the_pinned_one() {
    for codename in ["jammy", "noble", "bullseye", "bookworm", "chimera"] {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mock, log) = MockSystemOps::new(MockConfig::default());
        let downloader = MockDownloader::new(b"non e' il .deb vero".to_vec(), log.clone());

        let mut step = InstallWkhtmltopdf::with_parts(
            Box::new(mock),
            Box::new(downloader),
            default_checksums(),
            dir.path().to_path_buf(),
        );
        let c = ctx(codename);

        step.snapshot(&c).expect("snapshot");
        let err = step
            .run(&c)
            .expect_err("un .deb che non corrisponde al pin non si installa")
            .to_string();
        assert!(
            err.contains("checksum wkhtmltopdf non valido"),
            "deve fallire sul confronto, non prima ({codename}): {err}"
        );
        assert!(
            !ops_of(&log)
                .iter()
                .any(|op| matches!(op, Op::AptInstallDebFile(_))),
            "nessuna installazione con checksum non combaciante ({codename})"
        );
    }
}

/// La tabella di produzione deve coprire **ogni** suffisso che `map_codename`
/// può produrre: un buco qui rimetterebbe lo step nello stato "fallisce sempre".
#[test]
fn production_pins_cover_every_mapped_suffix() {
    let pins = default_checksums();
    assert_eq!(
        pins.keys().cloned().collect::<Vec<_>>(),
        vec![
            "bookworm".to_string(),
            "bullseye".to_string(),
            "jammy".to_string()
        ],
        "i .deb amd64 pubblicati da 0.12.6.1-3 sono esattamente questi tre"
    );

    for codename in [
        Some("jammy"),
        Some("noble"),
        Some("mantic"),
        Some("lunar"),
        Some("bookworm"),
        Some("bullseye"),
        Some("focal"),
        Some("chimera"),
        None,
    ] {
        let suffix = map_codename(codename).suffix;
        let pin = pins
            .get(&suffix)
            .unwrap_or_else(|| panic!("pin mancante per il suffisso '{suffix}' ({codename:?})"));
        assert_eq!(pin.len(), 64, "uno SHA-256 esadecimale ha 64 caratteri");
        assert!(
            pin.chars()
                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_uppercase()),
            "pin non esadecimale minuscolo: {pin}"
        );
    }
}

#[test]
fn missing_checksum_refuses_to_install() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = MockConfig::default();
    let (mock, log) = MockSystemOps::new(cfg);
    let downloader = MockDownloader::new(b"x".to_vec(), log.clone());

    // Tabella vuota: non è più il caso di produzione (i pin sono popolati), ma
    // il fail-closed va comunque provato — è la difesa se un suffisso futuro
    // finisse senza pin.
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        BTreeMap::new(),
        dir.path().to_path_buf(),
    );
    let c = ctx("jammy");

    step.snapshot(&c).expect("snapshot");
    assert!(
        step.run(&c).is_err(),
        "senza checksum atteso si rifiuta di installare (G3)"
    );
    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::AptInstallDebFile(_))),
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
    assert!(
        ops.is_empty(),
        "versione corretta già presente → nessuna azione, trovato: {ops:?}"
    );
}

#[test]
fn codename_mapping() {
    assert_eq!(map_codename(Some("noble")).suffix, "jammy");
    assert!(!map_codename(Some("noble")).fallback);
    assert_eq!(map_codename(Some("bookworm")).suffix, "bookworm");
    assert_eq!(map_codename(Some("bullseye")).suffix, "bullseye");

    let unknown = map_codename(Some("chimera"));
    assert_eq!(unknown.suffix, "jammy");
    assert!(
        unknown.fallback,
        "un codename ignoto usa jammy come fallback (con warning)"
    );

    let none = map_codename(None);
    assert_eq!(none.suffix, "jammy");
    assert!(none.fallback);

    // focal non ha più un ramo dedicato: il .deb `focal_amd64` non esiste nella
    // release 0.12.6.1-3, e Ubuntu 20.04 è già rifiutato da validate_os.
    let focal = map_codename(Some("focal"));
    assert_eq!(focal.suffix, "jammy");
    assert!(
        focal.fallback,
        "focal deve cadere nel fallback come ogni codename non mappato"
    );
}
