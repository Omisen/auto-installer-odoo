//! Test dei preflight checks (Fase 2): non mutanti, path iniettabili.

use std::io::Write;
use std::path::Path;

use odoo_installer::checks::{
    check_disk, check_os_from, check_ports, ensure_root_euid, ensure_sudo_user, format_python,
    format_release, is_newer_than_tested, parse_python_version, ports_to_check,
    python_is_newer_than_tested, untested_python_warning, untested_release_warning, validate_os,
    CheckError, OsInfo, NEWEST_TESTED_DEBIAN, NEWEST_TESTED_FEDORA, NEWEST_TESTED_PYTHON,
    NEWEST_TESTED_UBUNTU,
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

    // Fedora è supportata da M2, ma con la sua soglia: la 39 è sotto.
    let fedora = write_os_release(dir.path(), "ID=fedora\nVERSION_ID=\"39\"\n");
    assert!(matches!(
        check_os_from(&fedora),
        Err(CheckError::UnsupportedVersion { .. })
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

/// Una distribuzione che non trattiamo è già rifiutata da `OsFamily::from_os_id`:
/// darle una soglia superiore sarebbe un ramo che non può eseguire.
#[test]
fn an_unsupported_distribution_has_no_upper_threshold() {
    assert!(!is_newer_than_tested("arch", "99"));
}

// --- A-MD-5: l'avviso nomina la famiglia di chi lo legge --------------------

/// Il difetto, per esteso: su Fedora 44 l'installer stampava «(Ubuntu 24.04,
/// Debian 12)» — cioè due famiglie che non c'entrano — e **non** Fedora 41, la
/// sola release provata e l'unica informazione utile in quel momento.
///
/// È anche la guardia contro il ritorno del difetto: qualunque ricablatura del
/// testo che rinomini tutte e tre le famiglie fa fallire questo test, perché
/// l'asserzione non è «nomina la mia» ma «nomina la mia e **non** le altre».
#[test]
fn the_untested_warning_names_only_the_family_being_installed() {
    // Versione volutamente altissima: il test parla di *chi* viene nominato,
    // non di dove cade la soglia (quello lo dicono i test qui sopra).
    for (id, propria, estranee) in [
        ("ubuntu", "Ubuntu", ["Debian", "Fedora"]),
        ("debian", "Debian", ["Ubuntu", "Fedora"]),
        ("fedora", "Fedora", ["Ubuntu", "Debian"]),
    ] {
        let avviso = untested_release_warning(id, "99.99")
            .unwrap_or_else(|| panic!("{id} 99.99 è oltre ogni soglia: l'avviso deve esserci"));

        assert!(
            avviso.contains(propria),
            "su {id} l'avviso deve nominare {propria}, ma dice: {avviso}"
        );
        for altra in estranee {
            assert!(
                !avviso.contains(altra),
                "su {id} l'avviso nomina {altra}, che non c'entra nulla con questa \
                 installazione — è esattamente A-MD-5. Testo: {avviso}"
            );
        }
    }
}

/// **L'anello che mancava.** `the_newest_tested_releases_match_the_ci_matrix`
/// lega le costanti alla matrice della CI, ma nulla legava il *messaggio* alle
/// costanti: potevano divergere in silenzio, e l'hanno fatto per sette fasi.
///
/// La resa attesa è ricostruita qui a mano, di proposito, come per `KNOWN_KEYS`
/// in `ci_config.rs`: se il test riusasse la funzione di produzione proverebbe
/// solo che è uguale a se stessa.
#[test]
fn the_untested_warning_quotes_the_tested_release_from_the_constants() {
    fn come_la_scrive_la_distro((major, minor): (u32, u32)) -> String {
        if minor == 0 {
            format!("{major}")
        } else {
            format!("{major}.{minor:02}")
        }
    }

    for (id, costante) in [
        ("ubuntu", NEWEST_TESTED_UBUNTU),
        ("debian", NEWEST_TESTED_DEBIAN),
        ("fedora", NEWEST_TESTED_FEDORA),
    ] {
        let avviso = untested_release_warning(id, "99.99").expect("avviso presente");
        let attesa = come_la_scrive_la_distro(costante);
        assert!(
            avviso.contains(&attesa),
            "su {id} l'avviso deve citare la release provata ({attesa}, dalla costante \
             {costante:?}) invece di un numero scritto a mano. Testo: {avviso}"
        );
    }
}

/// Un avviso che comparisse su una release provata sarebbe un allarme falso, e
/// un allarme falso che compare sempre insegna a ignorare gli avvisi (A-V3-10).
#[test]
fn no_warning_on_a_release_we_actually_test() {
    for (id, version) in [
        ("ubuntu", "24.04"),
        ("ubuntu", "22.04"),
        ("debian", "12"),
        ("debian", "11"),
        ("fedora", "41"),
        ("fedora", "40"),
        // Famiglia ignota: già rifiutata a monte, qui non c'è nulla da dire.
        ("arch", "99"),
    ] {
        assert_eq!(
            untested_release_warning(id, version),
            None,
            "{id} {version} è fra quelle che proviamo (o non è affar nostro): \
             avvisare sarebbe un falso allarme"
        );
    }
}

/// `24.04`, non `24.4`: una versione Ubuntu scritta male in un avviso che parla
/// di versioni è il genere di dettaglio che fa dubitare del resto del messaggio.
///
/// Provata **direttamente**, perché il caso che rompe la formattazione ingenua
/// (un `minor` a due cifre) non è raggiungibile passando dalle costanti di oggi.
#[test]
fn a_release_is_rendered_the_way_the_distribution_writes_it() {
    assert_eq!(format_release((24, 4)), "24.04");
    assert_eq!(format_release((22, 4)), "22.04");
    assert_eq!(
        format_release((25, 10)),
        "25.10",
        "due cifre restano due cifre"
    );
    assert_eq!(
        format_release((12, 0)),
        "12",
        "«Debian 12.0» non lo dice nessuno"
    );
    assert_eq!(format_release((41, 0)), "41");
}

// --- L'interprete Python (A-MD-7) --------------------------------------------

/// Quello che `python3 --version` stampa davvero, incluse le forme che non sono
/// «tre numeri e basta».
///
/// Il caso `3.14` conta più degli altri: è Fedora 44, cioè la release che ha
/// fatto nascere questo controllo.
#[test]
fn the_interpreter_version_is_read_from_what_python_actually_prints() {
    assert_eq!(parse_python_version("Python 3.14.0\n"), Some((3, 14)));
    assert_eq!(parse_python_version("Python 3.12.3\n"), Some((3, 12)));
    assert_eq!(
        parse_python_version("Python 3.14.0rc1\n"),
        Some((3, 14)),
        "una release candidate è comunque quel minor"
    );
    assert_eq!(
        parse_python_version("Python 3.13\n"),
        Some((3, 13)),
        "due sole componenti sono un output legittimo"
    );
}

/// Un output che non si sa leggere dà `None`, **non** una versione di comodo.
///
/// La differenza è quella fra «so che è coperto» e «non lo so»: un `(0, 0)` di
/// ripiego sarebbe più basso di qualunque soglia, quindi silenzierebbe l'avviso
/// proprio quando non abbiamo idea di cosa ci sia sotto.
#[test]
fn an_unreadable_version_is_not_a_version() {
    assert_eq!(parse_python_version(""), None);
    assert_eq!(
        parse_python_version("bash: python3: command not found"),
        None
    );
    assert_eq!(parse_python_version("Python"), None);
    assert_eq!(parse_python_version("Python tre.quattordici"), None);
}

/// La soglia risponde in **entrambe le direzioni**, e il confine è incluso.
///
/// «Provata» vuol dire che su quella versione l'installazione arriva in fondo:
/// avvisare lì sarebbe un falso allarme, e un avviso che compare sempre insegna
/// a ignorare gli avvisi (A-V3-10).
#[test]
fn only_an_interpreter_newer_than_the_tested_one_is_flagged() {
    assert!(
        python_is_newer_than_tested((3, 14)),
        "3.14 (Fedora 44) è oltre la soglia: è il caso per cui il controllo esiste"
    );
    assert!(python_is_newer_than_tested((4, 0)));
    assert!(
        !python_is_newer_than_tested(NEWEST_TESTED_PYTHON),
        "sulla versione provata non c'è niente da segnalare"
    );
    assert!(!python_is_newer_than_tested((3, 12)));
    assert!(!python_is_newer_than_tested((3, 10)));
}

/// L'avviso nomina **il Python trovato e quello provato**, e dice cosa si
/// romperà.
///
/// È il contenuto a essere il valore del controllo, non il fatto che scatti: chi
/// legge deve poter decidere se andare avanti, e per farlo gli serve sapere qual
/// è il pezzo che salta (A-R9-1).
#[test]
fn the_python_warning_names_both_versions_and_what_will_break() {
    let avviso = untested_python_warning((3, 14)).expect("3.14 va segnalato");
    assert!(
        avviso.contains("3.14"),
        "l'avviso non dice quale Python ha trovato: {avviso}"
    );
    assert!(
        avviso.contains(&format_python(NEWEST_TESTED_PYTHON)),
        "l'avviso non cita la versione provata, quindi non si sa di quanto si è avanti: {avviso}"
    );
    assert!(
        avviso.contains("gevent"),
        "l'avviso non nomina il pacchetto che fallisce: {avviso}"
    );
    assert!(
        avviso.contains("install-python-requirements"),
        "l'avviso non dice dove si fermerà: {avviso}"
    );
    assert_eq!(
        untested_python_warning(NEWEST_TESTED_PYTHON),
        None,
        "sulla versione provata non c'è nessun avviso da emettere"
    );
}

/// `3.14`, non `3.140`: la formattazione delle release OS omette lo zero e
/// aggiunge le due cifre, quella di Python no. Sono due convenzioni diverse, e
/// riusare la funzione sbagliata scriverebbe «Python 3.14» come «3.014».
#[test]
fn a_python_version_is_rendered_the_way_python_writes_it() {
    assert_eq!(format_python((3, 14)), "3.14");
    assert_eq!(format_python((3, 9)), "3.9");
    assert_eq!(format_python((4, 0)), "4.0", "qui lo zero non si omette");
}
