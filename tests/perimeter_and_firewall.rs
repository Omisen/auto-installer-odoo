//! A-V3-7 e A-V3-8: due controlli che sembravano fare il loro lavoro.
//!
//! Sono finding diversi con la stessa forma: un confronto **troppo largo**.
//! Il primo chiedeva «questa stringa compare da qualche parte?» invece di
//! «questa regola c'è?»; il secondo confrontava due valori che arrivavano
//! **dallo stesso file**, quindi concordavano sempre.

use std::path::PathBuf;

use invok::distro::ufw::rule_in_status as ufw_rule_in_status;
use invok::state::{trust_verdict, InstallConfig};

// --- A-V3-7: `80/tcp` non è dentro `8080/tcp` --------------------------------

/// Output realistico di `ufw status`.
fn ufw_status(rules: &[&str]) -> String {
    let mut out = String::from("Status: active\n\nTo                         Action      From\n--                         ------      ----\n");
    for r in rules {
        out.push_str(&format!("{r:<26} ALLOW       Anywhere\n"));
    }
    out
}

/// **Il difetto.** `status.contains("80/tcp")` risponde `true` su una macchina
/// che ha solo `8080/tcp` — un'altra app web, un reverse proxy, un runner.
///
/// La conseguenza non è cosmetica: la regola per la 80 non entra nel delta, il
/// `run` non la apre, e nginx viene configurato e ricaricato correttamente ma
/// resta **irraggiungibile dall'esterno**. Nel report non c'è niente di anomalo
/// da leggere, ed è la parte peggiore.
#[test]
fn port_80_is_not_found_inside_port_8080() {
    let status = ufw_status(&["8080/tcp", "22/tcp"]);

    assert!(
        !ufw_rule_in_status(&status, "80/tcp"),
        "80/tcp NON è presente: è solo una sottostringa di 8080/tcp"
    );
    assert!(ufw_rule_in_status(&status, "8080/tcp"));
    assert!(ufw_rule_in_status(&status, "22/tcp"));
}

/// La variante IPv6 della stessa porta combacia: `ufw` la stampa come
/// `80/tcp (v6)`, ed è la stessa regola — riaprirla sarebbe un duplicato, e
/// rimuoverla all'undo toccherebbe qualcosa che non abbiamo aggiunto noi.
#[test]
fn the_ipv6_variant_is_the_same_rule() {
    let status = ufw_status(&["80/tcp", "80/tcp (v6)"]);
    assert!(ufw_rule_in_status(&status, "80/tcp"));
}

/// Una regola assente resta assente, anche su un output vuoto o inattivo.
#[test]
fn an_absent_rule_is_reported_absent() {
    assert!(!ufw_rule_in_status(&ufw_status(&[]), "443/tcp"));
    assert!(!ufw_rule_in_status("", "443/tcp"));
    assert!(!ufw_rule_in_status("Status: inactive\n", "443/tcp"));
}

/// L'intestazione di `ufw status` non deve mai combaciare con una regola.
#[test]
fn the_header_is_not_mistaken_for_a_rule() {
    let status = ufw_status(&["80/tcp"]);
    for header in ["To", "--", "Status:"] {
        assert!(
            !ufw_rule_in_status(&status, header),
            "'{header}' è intestazione, non una regola"
        );
    }
}

// --- A-V3-8: il perimetro ancorato a qualcosa che non arriva dal file --------

fn config_with(home: &str, install_dir: &str) -> InstallConfig {
    InstallConfig {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "odoo".to_string(),
        odoo_home: PathBuf::from(home),
        install_dir: PathBuf::from(install_dir),
        port: 8069,
        odoo_logfile: None,
        with_nginx: false,
        sudo_user: None,
        os_family: invok::distro::OsFamily::Debian,
        installer_version: None,
    }
}

/// **Il difetto.** La rete sul perimetro (`remove_created_root`) pretende che il
/// target stia sotto `home` — ma `home` e `target` arrivano **entrambi dal file
/// di stato**, quindi la guardia concordava sempre con sé stessa. Con
/// `odoo_home: "/"` un `created_root: "/etc"` passava senza obiezioni.
///
/// L'unico ancoraggio possibile è un valore che dal file **non** arriva.
#[test]
fn a_manifest_declaring_another_home_is_refused() {
    let bugiardo = config_with("/", "/etc");
    let err = bugiardo
        .validate_perimeter()
        .expect_err("un manifesto che dichiara '/' come home non è nostro");

    let msg = err.to_string();
    assert!(
        msg.contains("/opt/odoo"),
        "il messaggio deve dire qual è il perimetro vero: {msg}"
    );
}

/// Anche una home *plausibile* ma diversa viene rifiutata: `ODOO_HOME` è
/// dichiarata costante architetturale e non sovrascrivibile, quindi qualunque
/// altro valore descrive un'installazione che non abbiamo fatto noi.
#[test]
fn even_a_plausible_but_different_home_is_refused() {
    assert!(config_with("/srv/odoo", "/srv/odoo/odoo18")
        .validate_perimeter()
        .is_err());
}

/// La directory di installazione deve stare **sotto** la home, e non coincidere
/// con essa: coinciderebbe con l'intera home, e il suo undo la porterebbe via.
#[test]
fn the_install_dir_must_live_strictly_inside_the_home() {
    assert!(config_with("/opt/odoo", "/opt/altro")
        .validate_perimeter()
        .is_err());
    assert!(config_with("/opt/odoo", "/opt/odoo")
        .validate_perimeter()
        .is_err());
    assert!(config_with("/opt/odoo", "/opt/odoo/odoo18")
        .validate_perimeter()
        .is_ok());
}

// --- A-V3-8: il file di stato come fonte fidata ------------------------------

/// Il caso buono: root, `0600`, in una directory non scrivibile da terzi.
///
/// Verificabile solo perché la regola prende i permessi come parametri: un file
/// creato da un test appartiene all'utente che esegue i test, mai a root.
#[test]
fn a_root_owned_private_file_is_trusted() {
    assert!(trust_verdict(0, 0o100600, Some(0o40755)).is_ok());
    assert!(trust_verdict(0, 0o100640, Some(0o40750)).is_ok());
}

/// Un file di un altro utente non guida operazioni distruttive.
#[test]
fn a_file_owned_by_someone_else_is_refused() {
    let err = trust_verdict(1000, 0o100600, Some(0o40755)).expect_err("uid non-root");
    assert!(err.contains("root"), "{err}");
}

/// Scrivibile da gruppo o da altri: chi può riscriverlo sceglie cosa cancelliamo.
#[test]
fn a_world_or_group_writable_file_is_refused() {
    assert!(trust_verdict(0, 0o100666, Some(0o40755)).is_err());
    assert!(trust_verdict(0, 0o100620, Some(0o40755)).is_err());
}

/// La directory conta quanto il file: in una directory scrivibile da terzi il
/// file si **sostituisce** senza bisogno di poterlo modificare.
#[test]
fn a_file_in_a_world_writable_directory_is_refused() {
    let err = trust_verdict(0, 0o100600, Some(0o40777)).expect_err("directory aperta a tutti");
    assert!(err.contains("directory"), "{err}");
}

/// …a meno che non sia sticky: è esattamente ciò che lo sticky bit impedisce, e
/// `/tmp` è il caso di tutti i giorni. Un controllo che rifiutasse anche questo
/// bloccherebbe casi legittimi senza guadagnare nulla.
#[test]
fn the_sticky_bit_makes_a_shared_directory_acceptable() {
    assert!(trust_verdict(0, 0o100600, Some(0o41777)).is_ok());
}
