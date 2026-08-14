//! [`InstallWkhtmltopdf`]: the heart is G3 — a wrong checksum must fail the run
//! without installing anything.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{ops_of, MockConfig, MockDownloader, MockSystemOps, Op};
use invok::checks::OsInfo;
use invok::context::Context;
use invok::distro::OsFamily;
use invok::state::PreState;
use invok::step::Step;
use invok::steps::install_wkhtmltopdf::{
    default_checksums, map_package_suffix, InstallWkhtmltopdf,
};
use invok::system_ops::sha256_hex;

fn ctx(codename: &str) -> Context {
    Context {
        dry_run: false,
        os_info: Some(OsInfo {
            id: "ubuntu".to_string(),
            version: "22.04".to_string(),
            codename: Some(codename.to_string()),
            family: OsFamily::Debian,
        }),
        ..Default::default()
    }
}

/// the real SHA-256 of some bytes.
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

    // a table carrying a WRONG hash.
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
        !ops.iter()
            .any(|op| matches!(op, Op::PkgInstallLocalFile(_))),
        "con checksum errato NON si deve installare il .deb"
    );
}

#[test]
fn checksum_match_installs() {
    // the happy path for **each** suffix the release really publishes:
    // download, hash matches the pin, install through the manager.
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
        // the downloaded package is the mapped suffix's, not the codename's.
        assert!(
            ops.iter().any(|op| matches!(op, Op::Download { url, .. }
                if url.ends_with(&format!("wkhtmltox_0.12.6.1-3.{suffix}_amd64.deb")))),
            "{codename} deve scaricare il .deb {suffix}, trovato: {ops:?}"
        );
        assert!(
            ops.iter()
                .any(|op| matches!(op, Op::PkgInstallLocalFile(_))),
            "checksum valido → installa ({codename})"
        );
        assert_eq!(
            serde_json::from_value::<PreState>(step.snapshot_value()).expect("prestate"),
            PreState::CreatedByUs
        );
    }
}

/// A-RT-1, found on a minimal VM: the package depends on fonts and libraries a
/// clean machine lacks. installing it directly always failed — and left the
/// package database broken. installation must go through a command that
/// **resolves dependencies**.
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
        ops.iter().any(|op| matches!(op, Op::PkgInstallLocalFile(p)
            if p.to_string_lossy().ends_with(".deb"))),
        "il .deb va installato con apt, che risolve le dipendenze: {ops:?}"
    );
}

/// but a real error stays one: if the installation genuinely fails, the step
/// must fail and trigger the rollback.
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

/// the happy path against the **production table**: the pins are populated, so
/// the step really reaches the comparison. the mock package obviously differs
/// from the real one, so the comparison must fail without installing.
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
                .any(|op| matches!(op, Op::PkgInstallLocalFile(_))),
            "nessuna installazione con checksum non combaciante ({codename})"
        );
    }
}

/// the production table must cover **every** suffix the mapping can produce: a
/// hole here would put the step back into "always fails".
#[test]
fn production_pins_cover_every_mapped_suffix() {
    let pins = default_checksums();
    assert_eq!(
        pins.keys().cloned().collect::<Vec<_>>(),
        vec![
            "bookworm".to_string(),
            "bullseye".to_string(),
            "fedora37".to_string(),
            "jammy".to_string()
        ],
        "tre .deb amd64 e un .rpm x86_64: sono i pacchetti che 0.12.6.1-3 \
         pubblica e che una delle due famiglie può scegliere. Gli altri due rpm \
         (almalinux8/9) esistono ma nessun percorso li scarica"
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
        let suffix = map_package_suffix(&os_debian("ubuntu", codename)).suffix;
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

    // an empty table is no longer the production case, but the fail-closed path
    // must still be exercised: it is the defence if a future suffix ends up
    // without a pin.
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
            .any(|op| matches!(op, Op::PkgInstallLocalFile(_))),
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

/// builds an `OsInfo` of the deb family, for the mapping tests.
fn os_debian(id: &str, codename: Option<&str>) -> OsInfo {
    OsInfo {
        id: id.to_string(),
        version: "22.04".to_string(),
        codename: codename.map(|c| c.to_string()),
        family: OsFamily::Debian,
    }
}

#[test]
fn codename_mapping() {
    let ubuntu = |c| map_package_suffix(&os_debian("ubuntu", Some(c)));
    let debian = |c| map_package_suffix(&os_debian("debian", Some(c)));

    assert_eq!(ubuntu("noble").suffix, "jammy");
    assert!(!ubuntu("noble").fallback);
    assert_eq!(debian("bookworm").suffix, "bookworm");
    assert_eq!(debian("bullseye").suffix, "bullseye");

    // that codename has no dedicated branch: the release publishes no package
    // for it, and the OS is rejected upstream anyway.
    let focal = ubuntu("focal");
    assert_eq!(focal.suffix, "jammy");
    assert!(
        focal.fallback,
        "focal deve cadere nel fallback come ogni codename non mappato"
    );
}

/// **A5.2.** an unknown codename falls back to the newest package of **its own
/// family**, not to a single default.
///
/// not as theoretical as it looked: a newer Debian passes the version check —
/// the thresholds are open upwards, rightly — and would previously have taken a
/// package built for another distribution's system libraries. a fallback that
/// ignores the family is the wrong choice dressed as a default.
#[test]
fn an_unknown_codename_falls_back_within_its_own_family() {
    let debian_futura = map_package_suffix(&os_debian("debian", Some("trixie")));
    assert_eq!(
        debian_futura.suffix, "bookworm",
        "una Debian ignota deve prendere il pacchetto Debian più recente, non uno Ubuntu"
    );
    assert!(
        debian_futura.fallback,
        "resta un ripiego, e va detto nel log"
    );

    let ubuntu_futura = map_package_suffix(&os_debian("ubuntu", Some("questing")));
    assert_eq!(ubuntu_futura.suffix, "jammy");
    assert!(ubuntu_futura.fallback);

    // inside the family, an id that is not the Debian one falls back to the
    // Ubuntu package: the other half of that family.
    let senza_codename = map_package_suffix(&os_debian("ubuntu", None));
    assert_eq!(senza_codename.suffix, "jammy");
    assert!(senza_codename.fallback);
}

/// builds an `OsInfo` of the rpm family. the codename is the **empty string**,
/// as in the field: not absence but a value that means nothing, and the code
/// must not rest on it.
fn os_fedora(version: &str) -> OsInfo {
    OsInfo {
        id: "fedora".to_string(),
        version: version.to_string(),
        codename: Some(String::new()),
        family: OsFamily::Fedora,
    }
}

/// **on the rpm family the key changes nature**: there is no codename, so the
/// package is chosen by **version**.
#[test]
fn on_fedora_the_suffix_comes_from_the_version_not_the_codename() {
    let m = map_package_suffix(&os_fedora("41"));
    assert_eq!(
        m.suffix, "fedora37",
        "il codename vuoto non deve diventare il suffisso: la chiave è la versione"
    );
    assert!(
        !m.suffix.is_empty(),
        "un suffisso vuoto produrrebbe un URL che non esiste"
    );
}

/// **A5.2 on the second family.** any release takes the single package upstream
/// builds for it, but as a **declared fallback**.
///
/// the flag is not a detail: four releases of distance is a lot, and whoever
/// installs deserves to read in the log that the package was not built for
/// theirs. incompatible libraries would still make the manager refuse — a
/// package declares its own requirements — so the failure would be loud.
#[test]
fn any_other_fedora_falls_back_to_the_only_package_built_for_its_family() {
    let esatta = map_package_suffix(&os_fedora("37"));
    assert_eq!(esatta.suffix, "fedora37");
    assert!(
        !esatta.fallback,
        "sulla 37 il pacchetto è proprio il suo: non è un ripiego"
    );

    for version in ["38", "40", "41", "99"] {
        let m = map_package_suffix(&os_fedora(version));
        assert_eq!(m.suffix, "fedora37");
        assert!(
            m.fallback,
            "Fedora {version} non ha un pacchetto suo: è un ripiego, e va detto nel log"
        );
    }
}

/// the file name follows the **rpm** scheme, not the deb one.
///
/// two different upstream conventions, and getting it wrong would 404 the
/// download. checked against the really published assets.
#[test]
fn the_package_file_name_follows_the_format_of_its_family() {
    use invok::packaging::{apt::AptBackend, dnf::DnfBackend, PackageManager};

    assert_eq!(
        AptBackend.local_package_name("0.12.6.1-3", "jammy"),
        "wkhtmltox_0.12.6.1-3.jammy_amd64.deb"
    );
    assert_eq!(
        DnfBackend.local_package_name("0.12.6.1-3", "fedora37"),
        "wkhtmltox-0.12.6.1-3.fedora37.x86_64.rpm"
    );
}

/// **fail-closed on a family without pins**: until they are generated,
/// installation stops **before downloading**.
///
/// G3 applied to a whole family: installing a third-party binary without
/// knowing what it is, is exactly what the pin exists to prevent.
#[test]
fn a_family_without_pins_refuses_to_install_anything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mock, log) = MockSystemOps::new(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });
    let downloader = MockDownloader::new(b"qualsiasi cosa".to_vec(), log.clone());

    // deliberately **empty**: no longer the production case, both families have
    // pins, but this is the defence a third family would meet.
    let mut step = InstallWkhtmltopdf::with_parts(
        Box::new(mock),
        Box::new(downloader),
        BTreeMap::new(),
        dir.path().to_path_buf(),
    );
    let c = Context {
        dry_run: false,
        os_info: Some(os_fedora("41")),
        ..Default::default()
    };

    step.snapshot(&c).expect("snapshot");
    let err = step.run(&c).expect_err("senza pin non si installa");
    let msg = err.to_string();
    assert!(
        msg.contains("pin TOFU") && msg.contains("fedora"),
        "il messaggio deve dire che la famiglia non è tarata, non che manca un \
         suffisso: sono due azioni diverse. Trovato: {msg}"
    );
    assert!(
        !ops_of(&log)
            .iter()
            .any(|op| matches!(op, Op::Download { .. })),
        "il rifiuto arriva PRIMA del download: non si scarica ciò che non si sa verificare"
    );
}

/// A-V3-3: the package must not be born at a path anyone reading the source can
/// predict.
///
/// with a fixed name a local user could plant a symlink towards a system file
/// **before** the installer started, and have root write through it. the TOFU
/// pin protects the contents — a substituted package is refused — but not the
/// write, which happens before the check.
///
/// a constraint pulls the other way: the manager recognises a local path
/// **only** by its extension. the name must therefore be unpredictable *and*
/// keep it.
#[test]
fn the_downloaded_deb_has_an_unpredictable_path() {
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
    step.run(&c).expect("run");

    let ops = ops_of(&log);
    let dest = ops
        .iter()
        .find_map(|op| match op {
            Op::Download { dest, .. } => Some(dest.clone()),
            _ => None,
        })
        .expect("il download deve essere avvenuto");
    let nome = dest
        .file_name()
        .expect("nome")
        .to_string_lossy()
        .into_owned();

    assert_ne!(
        nome, "wkhtmltox_0.12.6.1-3.jammy_amd64.deb",
        "il nome del pacchetto è scritto nel sorgente: a quel path chiunque può \
         piazzare un symlink prima di noi (A-V3-3)"
    );
    assert!(
        nome.ends_with(".deb"),
        "apt non riconoscerebbe un percorso locale senza estensione .deb: {nome}"
    );
    assert!(
        nome.starts_with('.'),
        "il temporaneo deve essere nascosto: {nome}"
    );

    // the installed file is the downloaded one: we do not verify one file and
    // install another.
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::PkgInstallLocalFile(p) if *p == dest)),
        "apt deve installare esattamente il file verificato: {ops:?}"
    );
}

/// the pins are all **distinct**.
///
/// a pin's value cannot be checked offline — that would mean downloading the
/// file, and these tests have no network — so "this pin is the right one" is
/// out of the suite's reach. the field would notice, where a mismatch stops the
/// step before installing: loud, not silent.
///
/// one form of the error is caught here: the copy-paste that duplicates the
/// line above and forgets to change the value. two different packages do not
/// share a hash.
#[test]
fn every_pin_is_distinct() {
    let pins = default_checksums();
    let unici: std::collections::HashSet<&String> = pins.values().collect();
    assert_eq!(
        unici.len(),
        pins.len(),
        "due suffissi condividono lo stesso pin: file diversi non hanno lo stesso \
         SHA-256, quindi uno dei due è stato copiato dall'altro. Pin: {pins:?}"
    );
}
