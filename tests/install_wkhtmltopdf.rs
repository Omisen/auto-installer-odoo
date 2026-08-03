//! Test di [`InstallWkhtmltopdf`] (Fase 4): il cuore è G3 — un checksum errato
//! deve far fallire il run senza installare nulla.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{ops_of, MockConfig, MockDownloader, MockSystemOps, Op};
use odoo_installer::checks::OsInfo;
use odoo_installer::context::Context;
use odoo_installer::distro::OsFamily;
use odoo_installer::state::PreState;
use odoo_installer::step::Step;
use odoo_installer::steps::install_wkhtmltopdf::{
    default_checksums, map_package_suffix, InstallWkhtmltopdf,
};
use odoo_installer::system_ops::sha256_hex;

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
        !ops.iter()
            .any(|op| matches!(op, Op::PkgInstallLocalFile(_))),
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
        ops.iter().any(|op| matches!(op, Op::PkgInstallLocalFile(p)
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
                .any(|op| matches!(op, Op::PkgInstallLocalFile(_))),
            "nessuna installazione con checksum non combaciante ({codename})"
        );
    }
}

/// La tabella di produzione deve coprire **ogni** suffisso che `map_package_suffix`
/// può produrre: un buco qui rimetterebbe lo step nello stato "fallisce sempre".
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

/// Costruisce un `OsInfo` della famiglia Debian per i test di mappatura.
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

    // focal non ha un ramo dedicato: il .deb `focal_amd64` non esiste nella
    // release 0.12.6.1-3, e Ubuntu 20.04 è già rifiutato da validate_os.
    let focal = ubuntu("focal");
    assert_eq!(focal.suffix, "jammy");
    assert!(
        focal.fallback,
        "focal deve cadere nel fallback come ogni codename non mappato"
    );
}

/// **A5.2.** Un codename ignoto ricade sul pacchetto più recente della **sua
/// famiglia**, non su un default unico.
///
/// Il caso non era teorico come sembrava: Debian 13 (`trixie`) supera il
/// controllo di versione — le soglie sono aperte verso l'alto, ed è giusto che
/// lo siano — e prima si sarebbe preso un `.deb` costruito per **Ubuntu 22.04**,
/// con le librerie di sistema di un'altra distribuzione. Un fallback che ignora
/// la famiglia non è un ripiego prudente: è la scelta sbagliata travestita da
/// default.
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

    // Dentro la famiglia Debian, un `id` che non è "debian" ricade su `jammy`:
    // è il ripiego Ubuntu, e Ubuntu è l'altra metà di quella famiglia.
    let senza_codename = map_package_suffix(&os_debian("ubuntu", None));
    assert_eq!(senza_codename.suffix, "jammy");
    assert!(senza_codename.fallback);
}

/// Costruisce un `OsInfo` Fedora. Il codename è la **stringa vuota**, com'è in
/// campo: `/etc/os-release` di Fedora 41 dichiara `VERSION_CODENAME=""`, quindi
/// non è assenza — è un valore che non significa nulla, e il codice non deve
/// poggiarci sopra.
fn os_fedora(version: &str) -> OsInfo {
    OsInfo {
        id: "fedora".to_string(),
        version: version.to_string(),
        codename: Some(String::new()),
        family: OsFamily::Fedora,
    }
}

/// **Su Fedora la chiave cambia natura.** Il codename lì non esiste, quindi la
/// scelta del pacchetto passa dalla **versione**.
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

/// **A5.2 sulla seconda famiglia.** Una Fedora diversa dalla 37 prende comunque
/// `fedora37` — l'unico `.rpm` che upstream costruisce per questa famiglia — ma
/// come **ripiego dichiarato**.
///
/// Il `fallback: true` non è un dettaglio: quattro release di distanza sono
/// tante, e chi installa ha diritto di sapere dal log che il pacchetto non è
/// stato costruito per la sua. Se le librerie non tornassero sarebbe comunque
/// `dnf` a rifiutare — un `.rpm` dichiara i propri `Requires` — quindi il
/// fallimento sarebbe rumoroso, non silenzioso.
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

/// Il nome del file segue lo schema **rpm**, non quello deb.
///
/// Sono due convenzioni diverse di upstream (`wkhtmltox_{v}.{s}_amd64.deb`
/// contro `wkhtmltox-{v}.{s}.x86_64.rpm`), e sbagliarla darebbe un 404 al
/// download. Verificato contro gli asset realmente pubblicati.
#[test]
fn the_package_file_name_follows_the_format_of_its_family() {
    use odoo_installer::packaging::{apt::AptBackend, dnf::DnfBackend, PackageManager};

    assert_eq!(
        AptBackend.local_package_name("0.12.6.1-3", "jammy"),
        "wkhtmltox_0.12.6.1-3.jammy_amd64.deb"
    );
    assert_eq!(
        DnfBackend.local_package_name("0.12.6.1-3", "fedora37"),
        "wkhtmltox-0.12.6.1-3.fedora37.x86_64.rpm"
    );
}

/// **Fail-closed su una famiglia senza pin.** Finché i pin `.rpm` non sono stati
/// generati, l'installazione su Fedora si ferma **prima di scaricare**.
///
/// È G3 applicato a una famiglia intera: installare un binario di terze parti
/// senza sapere cosa si sta installando è precisamente ciò che il pin esiste per
/// impedire, e «funziona lo stesso» non è una ragione per aggirarlo.
#[test]
fn a_family_without_pins_refuses_to_install_anything() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (mock, log) = MockSystemOps::new(MockConfig {
        family: OsFamily::Fedora,
        ..MockConfig::default()
    });
    let downloader = MockDownloader::new(b"qualsiasi cosa".to_vec(), log.clone());

    // Tabella **vuota** di proposito: il caso di produzione non è più questo —
    // i pin ci sono per entrambe le famiglie — ma la difesa va provata comunque,
    // perché è quella che scatterebbe alla terza famiglia.
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

/// A-V3-3: il `.deb` non deve nascere a un percorso che chiunque legga il
/// sorgente possa prevedere.
///
/// Con `<tmp>/wkhtmltox_0.12.6.1-3.jammy_amd64.deb` un utente locale poteva
/// piazzare a quel path un symlink verso un file di sistema **prima** che
/// l'installer partisse, e farci scrivere sopra da root. Il pin TOFU protegge il
/// contenuto — un `.deb` sostituito viene rifiutato — ma non la scrittura, che
/// avviene prima della verifica.
///
/// Vincolo che tira nella direzione opposta: `apt-get install <file>` riconosce
/// un percorso locale **solo** dall'estensione `.deb`. Il nome deve quindi essere
/// imprevedibile *e* finire in `.deb`, e sono entrambe condizioni necessarie.
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

    // Il file installato è quello scaricato: non si verifica un file e se ne
    // installa un altro.
    assert!(
        ops.iter()
            .any(|op| matches!(op, Op::PkgInstallLocalFile(p) if *p == dest)),
        "apt deve installare esattamente il file verificato: {ops:?}"
    );
}

/// I pin sono tutti **distinti**.
///
/// Il valore di un pin non è verificabile offline — bisognerebbe scaricare il
/// file, e i test girano senza rete — quindi «questo pin è quello giusto» resta
/// fuori dalla portata della suite: se ne accorgerebbe il campo, dove uno sha
/// che non combacia ferma lo step con «checksum non valido (G3)» prima di
/// installare. Rumoroso, non silenzioso: il fail-closed protegge anche
/// dall'errore di trascrizione.
///
/// Una forma di quell'errore però si prende: il copia-incolla che duplica la
/// riga sopra e dimentica di cambiare il valore. Due pacchetti diversi non hanno
/// lo stesso SHA-256.
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
