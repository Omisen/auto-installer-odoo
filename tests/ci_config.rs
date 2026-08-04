//! Coerenza della configurazione usata dalla CI di integrazione (R5).
//!
//! `configs/ci.env`, `scripts/ci/integration-test.sh` e il parser `.env` devono
//! dire la stessa cosa. Se divergono, il sintomo arriva dopo **quaranta minuti**
//! di job — o peggio, non arriva affatto: una chiave scritta male non è un
//! errore, è un warning, e l'installer prosegue con i default. Il test di
//! integrazione andrebbe a verificare un database `odoo` che nessuno ha creato,
//! e passerebbe o fallirebbe per il motivo sbagliato.
//!
//! Questi test girano nella CI veloce (`test.yml`), su mock, in millisecondi.

use std::path::Path;

use odoo_installer::config::{self, ResolvedConfig};

const CI_ENV: &str = "configs/ci.env";
const CI_NGINX_ENV: &str = "configs/ci-nginx.env";
const CI_SCRIPT: &str = "scripts/ci/integration-test.sh";

/// Le chiavi che `config::parse_env_file` riconosce.
///
/// Duplicate qui di proposito, come **guardia**: la fonte di verità resta il
/// `match` in `config.rs`. Se qualcuno aggiunge una chiave a `ci.env` che il
/// parser ignorerebbe in silenzio, questo elenco la fa emergere subito.
const KNOWN_KEYS: &[&str] = &[
    "ODOO_VERSION",
    "ODOO_USER",
    "DB_USER",
    "DB_PASSWORD",
    "ODOO_PORT",
    "DB_NAME",
    "ODOO_INSTALL_DIR",
    "ODOO_ADMIN_PASSWD",
    "ODOO_LOGFILE",
    "WITH_NGINX",
    "NGINX_SERVER_NAME",
    "NGINX_OPEN_HTTPS_PORT",
    // Nome storico, ancora riconosciuto: vive nei `.env` dei clienti (A-V3-6).
    "NGINX_ENABLE_SSL",
];

fn resolve_ci_env() -> ResolvedConfig {
    let raw = config::parse_env_file(Path::new(CI_ENV)).expect("configs/ci.env deve esistere");
    let empty = config::RawConfig::default();
    // `interactive = false`: è come gira in CI, senza TTY. Il flag conta —
    // in non-interattivo la password debole 'admin' è un hard-stop.
    ResolvedConfig::resolve(&empty, &raw, &empty, false)
        .expect("configs/ci.env deve risolvere senza intervento interattivo")
}

#[test]
fn every_key_in_ci_env_is_understood_by_the_parser() {
    for file in [CI_ENV, CI_NGINX_ENV] {
        assert_keys_are_known(file);
    }
}

fn assert_keys_are_known(file: &str) {
    let content = std::fs::read_to_string(file).unwrap_or_else(|_| panic!("{file} deve esistere"));
    for (lineno, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let key = line
            .strip_prefix("export ")
            .unwrap_or(line)
            .split_once('=')
            .map(|(k, _)| k.trim())
            .unwrap_or_else(|| panic!("riga {} senza '=': {line}", lineno + 1));
        assert!(
            KNOWN_KEYS.contains(&key),
            "{file} riga {}: la chiave '{key}' non è riconosciuta dal parser .env. \
             Verrebbe ignorata con un warning e l'installer userebbe il default, \
             falsando il test di integrazione.",
            lineno + 1
        );
    }
}

/// B-V3-7: la config con nginx differisce da quella base **solo** per nginx.
///
/// Se divergessero anche su altro, un fallimento che compare solo nel job nginx
/// non sarebbe più attribuibile a nginx — e il valore di avere due job
/// identici-tranne-uno starebbe tutto lì.
#[test]
fn the_nginx_ci_config_differs_only_by_nginx() {
    let base = config::parse_env_file(Path::new(CI_ENV)).expect("ci.env");
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");

    assert_eq!(base.version, con_nginx.version);
    assert_eq!(base.odoo_user, con_nginx.odoo_user);
    assert_eq!(base.db_name, con_nginx.db_name);
    assert_eq!(base.db_user, con_nginx.db_user);
    assert_eq!(base.port, con_nginx.port);
    assert_eq!(base.admin_passwd, con_nginx.admin_passwd);
    assert_eq!(base.logfile, con_nginx.logfile);

    assert_eq!(
        base.with_nginx,
        Some(false),
        "la config base resta senza nginx"
    );
    assert_eq!(
        con_nginx.with_nginx,
        Some(true),
        "è l'unica ragione per cui questo file esiste"
    );
}

/// Il flag che apre la 443 resta **fuori** dalla config di CI, e non per caso:
/// su un runner `ufw` è installato ma inattivo, quindi lo step uscirebbe subito
/// senza aggiungere copertura — e il flag comunque non tocca il vhost (A-V3-6).
#[test]
fn the_nginx_ci_config_does_not_ask_for_the_https_port() {
    let con_nginx = config::parse_env_file(Path::new(CI_NGINX_ENV)).expect("ci-nginx.env");
    assert_eq!(con_nginx.open_https_port, None);
}

#[test]
fn ci_env_resolves_to_what_the_integration_script_expects() {
    let cfg = resolve_ci_env();

    // Il nome del database è il perno del test: NON deve essere il default.
    // Se il rollback ricavasse i nomi dai default invece che dalla config
    // persistita (regressione A-R4-1), cercherebbe 'odoo' e lascerebbe
    // 'citest' sul sistema — e il test di pulizia lo vedrebbe.
    assert_eq!(cfg.db_name, "citest");
    assert_ne!(
        cfg.db_name, "odoo",
        "il db_name della CI deve differire dal default, altrimenti il test non \
         distingue 'il rollback ha usato la config persistita' da 'ha indovinato'"
    );

    assert_eq!(cfg.version, "18.0");
    assert_eq!(cfg.version_short, "18");
    assert_eq!(cfg.odoo_user, "odoo");
    assert_eq!(cfg.db_user, "odoo");
    assert_eq!(cfg.port, 8069);
    assert!(!cfg.with_nginx, "la sonda di CI non configura Nginx");
    assert!(
        cfg.odoo_logfile.is_none(),
        "ODOO_LOGFILE vuoto = log su journal: nessuna log dir da verificare"
    );
    assert!(
        cfg.db_password.is_empty(),
        "password vuota = autenticazione peer, il percorso che vogliamo esercitare"
    );
}

#[test]
fn the_ci_admin_password_is_not_the_weak_default() {
    // In non-interattivo `resolve` rifiuta la password 'admin'
    // (InsecureAdminNonInteractive): con quel valore la CI non partirebbe
    // nemmeno. `resolve_ci_env` lo prova già col suo `expect`; qui si dice
    // perché, così chi tocca il file sa cosa sta toccando.
    let cfg = resolve_ci_env();
    assert_ne!(
        cfg.admin_passwd.expose(),
        "admin",
        "una password 'admin' fa fallire l'installazione non interattiva prima \
         di qualsiasi step"
    );
}

#[test]
fn the_integration_script_and_ci_env_agree_on_the_artifacts() {
    // I default dello script devono combaciare coi valori del .env: lo script
    // verifica *per nome* il database, l'utente e la porta, e un disallineamento
    // renderebbe le asserzioni vacue (cercherebbero artefatti che nessuno ha
    // creato) invece di farle fallire.
    let script = std::fs::read_to_string(CI_SCRIPT).expect("scripts/ci/integration-test.sh");
    let cfg = resolve_ci_env();

    for (var, expected) in [
        ("DB_NAME", cfg.db_name.clone()),
        ("DB_ROLE", cfg.db_user.clone()),
        ("OS_USER", cfg.odoo_user.clone()),
        ("PORT", cfg.port.to_string()),
        ("VER_SHORT", cfg.version_short.clone()),
    ] {
        let needle = format!("{var}:-{expected}}}");
        assert!(
            script.contains(&needle),
            "lo script di integrazione deve avere `${{{var}:-{expected}}}` per \
             combaciare con configs/ci.env (atteso il frammento `{needle}`)"
        );
    }
}

#[test]
fn the_integration_script_is_executable_and_syntactically_valid() {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(CI_SCRIPT).expect("lo script deve esistere");
    assert!(
        meta.permissions().mode() & 0o111 != 0,
        "lo script di integrazione deve avere il bit di esecuzione"
    );

    // `bash -n` non esegue nulla: analizza soltanto la sintassi. Uno script
    // rotto verrebbe scoperto altrimenti solo a metà di un job da 40 minuti,
    // dopo aver già installato mezzo sistema.
    let status = std::process::Command::new("bash")
        .args(["-n", CI_SCRIPT])
        .status()
        .expect("bash deve essere disponibile");
    assert!(status.success(), "sintassi non valida in {CI_SCRIPT}");
}

// --- A5.3: le costanti "ultima release provata" seguono la CI vera ----------

const WORKFLOW: &str = ".github/workflows/integration.yml";

/// `NEWEST_TESTED_*` dicono all'utente su cosa
/// l'installer viene provato davvero. Se divergessero dalla matrice della CI,
/// l'avviso mentirebbe in una delle due direzioni: tacerebbe su una release non
/// provata, o allarmerebbe su una che proviamo.
///
/// La fonte di verità resta il workflow; queste costanti la inseguono, e questo
/// test lo rende obbligatorio invece che auspicabile.
#[test]
fn the_newest_tested_releases_match_the_ci_matrix() {
    use odoo_installer::checks::{
        NEWEST_TESTED_DEBIAN, NEWEST_TESTED_FEDORA, NEWEST_TESTED_UBUNTU,
    };

    let wf = std::fs::read_to_string(WORKFLOW).expect("il workflow di integrazione deve esistere");

    // Ubuntu: `os: [ubuntu-22.04, ubuntu-24.04]` più i `runs-on:` dei job
    // singoli, che non passano dalla matrice.
    let ubuntu_max = versions_in(&wf, "ubuntu-")
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Ubuntu");
    assert_eq!(
        ubuntu_max, NEWEST_TESTED_UBUNTU,
        "la CI gira su Ubuntu {ubuntu_max:?} ma la costante dice {NEWEST_TESTED_UBUNTU:?}: \
         l'avviso su release non testate direbbe il falso"
    );

    // Debian: `image: ["debian:12", "debian:11"]`.
    let debian_max = versions_in(&wf, "debian:")
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Debian");
    assert_eq!(
        debian_max, NEWEST_TESTED_DEBIAN,
        "la CI gira su Debian {debian_max:?} ma la costante dice {NEWEST_TESTED_DEBIAN:?}"
    );

    // Fedora, e qui la matrice dice DUE cose diverse.
    //
    // Ci sono voci bloccanti (un rosso ferma tutto) e una SONDA su una release
    // mai supportata, tollerata in rosso perché un rosso che ci si aspetta di
    // vedere insegna a ignorare i rossi. La costante deve seguire le sole voci
    // bloccanti: `is_newer_than_tested` promette «release su cui l'installer
    // viene provato», e una release il cui fallimento non ferma nessuno non è
    // provata — è osservata. Contarla direbbe il falso proprio nel senso che
    // rende l'avviso utile.
    let fedora_bloccanti = versions_in(&senza_sonde(&wf), "fedora:");
    let fedora_max = fedora_bloccanti
        .into_iter()
        .max()
        .expect("la CI deve girare su almeno una Fedora bloccante");
    assert_eq!(
        fedora_max, NEWEST_TESTED_FEDORA,
        "la CI gira su Fedora {fedora_max:?} ma la costante dice {NEWEST_TESTED_FEDORA:?}"
    );

    // E il marcatore non dev'essere un modo per zittire la guardia: una sonda
    // ha senso solo su una release PIÙ RECENTE di quelle provate davvero.
    // Marcare una voce bloccante come sonda la farebbe sparire dal confronto
    // qui sopra senza che nulla lo dica — la stessa forma del difetto che
    // questo test esiste per impedire, un livello più su.
    for sonda in versions_in(&sole_sonde(&wf), "fedora:") {
        assert!(
            sonda > NEWEST_TESTED_FEDORA,
            "la sonda su Fedora {sonda:?} non è più recente di {NEWEST_TESTED_FEDORA:?}: \
             o è una voce bloccante marcata per sbaglio come sonda, o la costante è rimasta \
             indietro rispetto a una release ormai provata davvero"
        );
    }
}

/// Il workflow senza le righe marcate come sonda non bloccante.
///
/// Testuale e non strutturato, come tutto il resto di questa guardia: il test
/// legge il file com'è, senza portarsi dietro un parser YAML per una domanda
/// che si risolve guardando una riga.
fn senza_sonde(wf: &str) -> String {
    wf.lines()
        .filter(|riga| !riga.contains(MARCATORE_SONDA))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Solo le righe marcate come sonda.
fn sole_sonde(wf: &str) -> String {
    wf.lines()
        .filter(|riga| riga.contains(MARCATORE_SONDA))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Il commento che marca una voce di matrice come sonda tollerata in rosso.
/// Deve combaciare con `.github/workflows/integration.yml`.
const MARCATORE_SONDA: &str = "sonda-non-bloccante";

/// Tutte le versioni che seguono `prefisso` nel testo, come `(major, minor)`.
fn versions_in(text: &str, prefisso: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (_, resto) in text
        .match_indices(prefisso)
        .map(|(i, m)| (i, &text[i + m.len()..]))
    {
        let numero: String = resto
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if numero.is_empty() {
            continue;
        }
        let mut parti = numero.split('.');
        let major = parti.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let minor = parti.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        out.push((major, minor));
    }
    out
}

// --- M5: le due confezioni devono contenere la stessa cosa ------------------

/// `.deb` e `.rpm` depositano **lo stesso binario nello stesso posto**.
///
/// I due strumenti (`cargo deb`, `cargo generate-rpm`) non si parlano e leggono
/// blocchi di metadati diversi: la lista degli asset è scritta due volte, in due
/// sintassi diverse, nello stesso file. Due liste che devono coincidere e
/// nessuno che lo verifichi è il modo in cui si finisce per pubblicare un `.rpm`
/// che non contiene il binario — e nessuno se ne accorge finché un utente non
/// prova a installarlo.
///
/// Non si verifica il *contenuto* dei pacchetti (servirebbe generarli, e i due
/// strumenti non sono installati qui): si verifica che le due dichiarazioni
/// promettano la stessa cosa.
#[test]
fn the_deb_and_the_rpm_ship_the_same_binary() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");

    let blocco = |intestazione: &str| -> String {
        let inizio = manifest
            .find(intestazione)
            .unwrap_or_else(|| panic!("manca il blocco {intestazione}"));
        let resto = &manifest[inizio + intestazione.len()..];
        let fine = resto.find("\n[").unwrap_or(resto.len());
        resto[..fine].to_string()
    };

    let deb = blocco("[package.metadata.deb]");
    let rpm = blocco("[package.metadata.generate-rpm]");

    for (nome, testo, destinazione) in [
        // cargo-deb: ["sorgente", "destinazione/", "mode"]
        ("deb", &deb, "usr/bin/"),
        // cargo-generate-rpm: { source, dest, mode }
        ("rpm", &rpm, "/usr/bin/odoo-installer"),
    ] {
        assert!(
            testo.contains("target/release/odoo-installer"),
            "{nome}: deve impacchettare il binario compilato, non altro"
        );
        assert!(
            testo.contains(destinazione),
            "{nome}: il binario deve finire in /usr/bin, o il comando non è nel PATH"
        );
        assert!(
            testo.contains("755"),
            "{nome}: un binario senza bit di esecuzione non è un binario"
        );
        assert!(
            testo.contains("README"),
            "{nome}: la doc accompagna il pacchetto in entrambe le confezioni"
        );
    }

    // La promessa che vale per tutte e due: impacchettano il TOOL, non Odoo.
    // Nessun servizio, nessuna dipendenza su postgres/nginx — quelle le crea
    // l'installer a runtime, ed è ciò che rende il pacchetto innocuo da
    // installare.
    //
    // Si guardano le **righe di dichiarazione**, non il testo intero: la
    // descrizione nomina PostgreSQL e nginx a ragione (l'installer li
    // configura), e un controllo che cercasse quelle parole ovunque
    // fallirebbe su una frase corretta. Prima versione di questo test: proprio
    // così.
    let dichiarazioni = |testo: &str, chiavi: &[&str]| -> String {
        testo
            .lines()
            .filter(|riga| {
                let r = riga.trim_start();
                chiavi.iter().any(|k| r.starts_with(k))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    for (nome, testo) in [("deb", &deb), ("rpm", &rpm)] {
        assert!(
            !testo.contains("systemd-units"),
            "{nome}: il pacchetto non installa servizi"
        );
        let deps = dichiarazioni(testo, &["depends", "requires", "recommends", "suggests"]);
        assert!(
            !deps.contains("postgresql") && !deps.contains("nginx") && !deps.contains("python"),
            "{nome}: nessuna dipendenza su ciò che l'installer gestisce a runtime, \
             trovato: {deps}"
        );
    }
}

/// I comandi di installazione del README puntano alla versione **di questo
/// pacchetto**, non a una passata.
///
/// Il README non usa più variabili di shell (`VER=…`): i comandi sono stringhe
/// intere, copiabili senza leggerle. È la forma giusta per chi installa — e
/// moltiplica per nove i punti in cui una versione può restare indietro. Due
/// fonti che devono coincidere e nessuno che lo verifichi è il modo in cui si
/// finisce per far scaricare ai clienti la release precedente, in silenzio: il
/// comando funziona, il file esiste, e nessuno se ne accorge.
///
/// Si verifica la corrispondenza con `Cargo.toml`, che è la versione che il
/// workflow di release taggherà — quindi il README è aggiornato quando lo è il
/// manifesto, e non «quando qualcuno si ricorda».
#[test]
fn the_readme_download_commands_point_at_this_version() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let versione = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml deve dichiarare una versione");
    let readme = std::fs::read_to_string("README.md").expect("leggo README.md");

    // Ogni URL di download nomina QUESTA versione.
    let mut url = 0;
    for pezzo in readme.split("releases/download/v").skip(1) {
        url += 1;
        let trovata = pezzo.split('/').next().unwrap_or("");
        assert_eq!(
            trovata, versione,
            "il README scarica dalla release v{trovata} mentre il pacchetto è {versione}"
        );
    }
    assert!(
        url >= 6,
        "attesi almeno sei link di download (tar.gz, .deb, .rpm, con i rispettivi .sha256), \
         trovati {url}: se la sezione è cambiata, questa guardia va aggiornata insieme"
    );

    // E i nomi dei file sono quelli che le due confezioni producono davvero.
    for atteso in [
        format!("odoo-installer_{versione}_amd64.deb"),
        format!("odoo-installer-{versione}-1.x86_64.rpm"),
    ] {
        assert!(
            readme.contains(&atteso),
            "il README non nomina `{atteso}`: il comando di installazione scaricherebbe un file \
             che quella release non contiene"
        );
    }
}

/// La versione che il binario dichiara è quella di `Cargo.toml`.
///
/// `INSTALLER_VERSION` viene da `env!("CARGO_PKG_VERSION")`, quindi oggi non può
/// divergere — e questo test esiste proprio per il giorno in cui qualcuno,
/// volendo «renderla configurabile», la trasformasse in una costante scritta a
/// mano. È la stessa guardia del README, applicata al terzo consumatore della
/// versione: flag, log e manifesto devono dire tutti lo stesso numero (A-V3-16).
#[test]
fn the_version_the_binary_reports_is_the_one_in_the_manifest() {
    let manifest = std::fs::read_to_string("Cargo.toml").expect("leggo Cargo.toml");
    let dichiarata = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|l| l.split('"').next())
        .expect("Cargo.toml deve dichiarare una versione");

    assert_eq!(
        odoo_installer::INSTALLER_VERSION,
        dichiarata,
        "il binario dice di essere {} ma il pacchetto è {dichiarata}",
        odoo_installer::INSTALLER_VERSION
    );
}
