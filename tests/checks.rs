//! Test dei preflight checks (Fase 2): non mutanti, path iniettabili.

use std::io::Write;
use std::path::Path;

use odoo_installer::checks::{
    check_disk, check_os_from, check_ports, ensure_root_euid, ensure_sudo_user,
    is_newer_than_tested, ports_to_check, validate_os, CheckError, OsInfo,
};
use odoo_installer::distro::OsFamily;

fn write_os_release(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("os-release");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

// --- Disco: NON deve creare la directory (C4) --------------------------------

#[test]
fn check_disk_does_not_create_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Target annidato e inesistente: la misura deve risalire all'antenato.
    let target = dir.path().join("opt").join("odoo");
    assert!(!target.exists());

    // required_gb = 0 → passa sempre; ci interessa il non-effetto collaterale.
    check_disk(&target, 0).expect("check_disk ok");

    // Il fix di C4: nessuna directory è stata creata per misurare.
    assert!(!target.exists(), "check_disk NON deve creare il target");
    assert!(
        !dir.path().join("opt").exists(),
        "check_disk NON deve creare neppure gli intermedi"
    );
}

#[test]
fn check_disk_reports_insufficient_without_creating() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("opt").join("odoo");

    // Soglia irraggiungibile → errore tipizzato, ma sempre senza creare nulla.
    let err = check_disk(&target, u64::MAX).expect_err("deve fallire");
    assert!(matches!(err, CheckError::InsufficientDisk { .. }));
    assert!(!target.exists());
}

// --- OS: parsing + soglie di versione ---------------------------------------

#[test]
fn check_os_supported() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ubuntu = write_os_release(
        dir.path(),
        "ID=ubuntu\nVERSION_ID=\"22.04\"\nVERSION_CODENAME=jammy\n",
    );
    let info = check_os_from(&ubuntu).expect("ubuntu 22.04 ok");
    assert_eq!(info.id, "ubuntu");
    assert_eq!(info.version, "22.04");
    assert_eq!(info.codename.as_deref(), Some("jammy"));

    let debian = write_os_release(
        dir.path(),
        "ID=debian\nVERSION_ID=\"12\"\nVERSION_CODENAME=bookworm\n",
    );
    let info = check_os_from(&debian).expect("debian 12 ok");
    assert_eq!(info.id, "debian");
}

#[test]
fn check_os_rejects_old_and_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");

    let old_ubuntu = write_os_release(dir.path(), "ID=ubuntu\nVERSION_ID=\"20.04\"\n");
    assert!(matches!(
        check_os_from(&old_ubuntu),
        Err(CheckError::UnsupportedVersion { .. })
    ));

    let old_debian = write_os_release(dir.path(), "ID=debian\nVERSION_ID=\"10\"\n");
    assert!(matches!(
        check_os_from(&old_debian),
        Err(CheckError::UnsupportedVersion { .. })
    ));

    // Fedora è una famiglia **conosciuta ma non ancora supportata**: il rifiuto
    // c'è, ma con un errore diverso da «non supportato». Dire «non ancora» a chi
    // ha una Fedora e «non supportato» a chi ha una Arch sono due informazioni
    // diverse, e la seconda manderebbe a cercare una soluzione che non esiste.
    let fedora = write_os_release(dir.path(), "ID=fedora\nVERSION_ID=\"39\"\n");
    assert!(matches!(
        check_os_from(&fedora),
        Err(CheckError::NotYetSupportedOs { .. })
    ));

    // Una distribuzione di cui non conosciamo nemmeno la famiglia resta
    // `UnsupportedOs`, ed è respinta da `OsFamily::from_os_id` — l'unico gate.
    let arch = write_os_release(dir.path(), "ID=arch\nVERSION_ID=\"rolling\"\n");
    assert!(matches!(
        check_os_from(&arch),
        Err(CheckError::UnsupportedOs { .. })
    ));

    // File assente → errore dedicato.
    let missing = dir.path().join("nope").join("os-release");
    assert!(matches!(
        check_os_from(&missing),
        Err(CheckError::OsReleaseNotFound(_))
    ));
}

// --- Root / sudo: logica pura, testabile senza privilegi ---------------------

#[test]
fn root_and_sudo_pure_logic() {
    assert!(ensure_root_euid(0).is_ok());
    assert!(matches!(
        ensure_root_euid(1000),
        Err(CheckError::NotRoot { euid: 1000 })
    ));

    assert!(ensure_sudo_user(Some("alice")).is_ok());
    assert!(matches!(
        ensure_sudo_user(None),
        Err(CheckError::NoSudoUser)
    ));
    assert!(matches!(
        ensure_sudo_user(Some("")),
        Err(CheckError::NoSudoUser)
    ));
}

/// A-V3-15: con `--with-nginx` su una macchina dove **nginx sta già servendo**,
/// la porta 80 non è un conflitto — è del programma che stiamo per configurare.
///
/// Il controllo pretendeva la 80 libera ogni volta che si chiedeva
/// `--with-nginx`, e rendeva così impossibile il caso d'uso normale: aggiungere
/// un vhost Odoo a un reverse proxy esistente. Gli step nginx lo gestiscono
/// esplicitamente — `NginxInstall` marca `Preexisting` un nginx già attivo e non
/// lo tocca — quindi il preflight rifiutava un'installazione che il resto del
/// programma sa fare benissimo.
///
/// Trovato costruendo il job di CI con nginx (B-V3-7): la zona non era mai stata
/// eseguita, e alla prima esecuzione reale si sarebbe fermata qui.
///
/// Si verifica la **decisione** — quali porte guardare — e non l'esito della
/// sonda: l'esito dipende da cosa gira sulla macchina che esegue i test, e su
/// una dove la 80 è libera un controllo sbagliato passerebbe lo stesso. La
/// prima versione di questo test faceva esattamente quell'errore e la mutazione
/// di prova gli è sopravvissuta.
#[test]
fn port_80_held_by_a_running_nginx_is_not_a_conflict() {
    // nginx già in ascolto: 80 e 443 non si guardano affatto.
    assert_eq!(
        ports_to_check(8069, true, /* nginx_already_serving */ true),
        vec![8069],
        "un nginx che già serve non è un conflitto con sé stesso"
    );

    // nginx richiesto ma non ancora in ascolto: il conflitto sarebbe reale.
    assert_eq!(ports_to_check(8069, true, false), vec![8069, 80, 443]);

    // Senza nginx, la 80 non riguarda nessuno.
    assert_eq!(ports_to_check(8069, false, false), vec![8069]);
    assert_eq!(
        ports_to_check(8069, false, true),
        vec![8069],
        "senza --with-nginx lo stato di nginx è irrilevante"
    );
}

/// Ma se nginx **non** sta servendo, un conflitto sulla 80 resta un conflitto:
/// lì nginx non riuscirebbe nemmeno a fare il bind, e dirlo al preflight è
/// meglio che scoprirlo al reload.
#[test]
fn an_occupied_port_still_stops_the_installation() {
    use std::net::TcpListener;

    // Si occupa una porta davvero e la si passa come "porta Odoo": esercita la
    // sonda senza dover fare il bind sulla 80, che in un test senza privilegi
    // non si può.
    let occupata = TcpListener::bind("127.0.0.1:0").expect("bind");
    let porta = occupata.local_addr().expect("addr").port();

    assert!(
        check_ports(porta, false, false).is_err(),
        "una porta occupata da terzi deve fermare l'installazione"
    );
}

// --- A5.3: accettare una release non testata, ma dirlo ----------------------

/// Le soglie di `validate_os` sono aperte verso l'alto — e devono restarci: un
/// rifiuto senza prova blocca il caso buono, e un'installazione impedita è un
/// danno certo mentre quello evitato è ipotetico (lezione di A5.1-bis).
///
/// Ma «accettiamo» non deve voler dire «tacciamo»: su una release più recente di
/// quelle che proviamo davvero, l'utente ha diritto di saperlo — è
/// l'informazione che gli serve quando qualcosa va storto.
#[test]
fn a_release_newer_than_the_tested_ones_is_flagged() {
    // Ubuntu: 24.04 è l'ultima provata.
    assert!(!is_newer_than_tested("ubuntu", "22.04"));
    assert!(!is_newer_than_tested("ubuntu", "24.04"));
    assert!(is_newer_than_tested("ubuntu", "24.10"));
    assert!(is_newer_than_tested("ubuntu", "26.04"));

    // Debian: 12 è l'ultima provata.
    assert!(!is_newer_than_tested("debian", "11"));
    assert!(!is_newer_than_tested("debian", "12"));
    assert!(
        is_newer_than_tested("debian", "13"),
        "Debian 13 supera le soglie e va segnalata: è lo stesso scenario che in \
         A5.2 si prendeva un pacchetto Ubuntu"
    );
}

/// Restare **accettate**: la segnalazione è un avviso, non un rifiuto.
#[test]
fn a_newer_release_is_still_accepted() {
    for (id, version) in [("ubuntu", "26.04"), ("debian", "13")] {
        let info = OsInfo {
            id: id.to_string(),
            version: version.to_string(),
            codename: None,
            family: OsFamily::Debian,
        };
        assert!(
            validate_os(&info).is_ok(),
            "{id} {version} dev'essere accettata: bloccarla sarebbe un danno certo \
             per evitarne uno ipotetico"
        );
        assert!(is_newer_than_tested(id, version), "…ma con un avviso");
    }
}

/// Una distribuzione che non trattiamo è già rifiutata da `validate_os`: darle
/// una soglia superiore sarebbe un ramo che non può eseguire.
#[test]
fn an_unsupported_distribution_has_no_upper_threshold() {
    assert!(!is_newer_than_tested("fedora", "99"));
}
