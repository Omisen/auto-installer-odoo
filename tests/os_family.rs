//! M0 — la famiglia della distribuzione si **rilegge**, non si rideduce.
//!
//! Questi test presidiano un difetto che oggi non esiste ancora, e che esiste
//! per non farlo nascere: quando ci saranno due gestori di pacchetti, l'`undo`
//! del delta dovrà sapere quale invocare, e l'unica fonte accettabile è il
//! manifesto scritto dall'installazione che quegli artefatti li ha creati.
//!
//! Il punto più delicato è uno solo — `InstallConfig::to_context` — e c'è un
//! test scritto apposta per morire se quella riga sparisce, perché il difetto
//! sarebbe altrimenti **silenzioso**: la famiglia ricadrebbe sul default
//! `Debian` e in campo si vedrebbe solo un `apt-get` che fallisce su una
//! macchina senza apt.

use std::io::Write;
use std::path::{Path, PathBuf};

use odoo_installer::checks::{check_os_from, os_id_from, validate_os, CheckError, OsInfo};
use odoo_installer::context::Context;
use odoo_installer::distro::{family_mismatch, OsFamily};
use odoo_installer::state::{
    start_decision, InstallConfig, InstallState, PreState, StartDecision, StepRecord,
};

// --- helper ------------------------------------------------------------------

fn write_os_release(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("os-release");
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    path
}

fn config_for(family: OsFamily) -> InstallConfig {
    InstallConfig {
        odoo_version: "18.0".to_string(),
        odoo_version_short: "18".to_string(),
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "citest".to_string(),
        odoo_home: PathBuf::from("/opt/odoo"),
        install_dir: PathBuf::from("/opt/odoo/odoo18"),
        port: 8069,
        odoo_logfile: None,
        with_nginx: false,
        sudo_user: None,
        os_family: family,
    }
}

// --- La derivazione: un gate solo, e nessun ripiego --------------------------

#[test]
fn the_family_comes_from_the_os_id_and_nowhere_else() {
    assert_eq!(OsFamily::from_os_id("ubuntu"), Some(OsFamily::Debian));
    assert_eq!(OsFamily::from_os_id("debian"), Some(OsFamily::Debian));
    assert_eq!(OsFamily::from_os_id("fedora"), Some(OsFamily::Fedora));

    // Una distribuzione che non trattiamo non ha famiglia: `None`, non un
    // ripiego. È l'unico posto in cui si decide che una distro ci è ignota, e
    // deve poter dire di no.
    assert_eq!(OsFamily::from_os_id("arch"), None);
    assert_eq!(OsFamily::from_os_id(""), None);
}

/// `ID_LIKE=fedora` **non** apre la porta alle derivate.
///
/// Rocky, AlmaLinux e CentOS Stream lo dichiarano: leggerlo le farebbe entrare
/// senza che nessuno le abbia mai provate. Per una famiglia nuova si parte
/// chiusi — e non è in contraddizione con A5.1-bis, che riguarda il non
/// respingere release *più recenti* di una famiglia già supportata.
#[test]
fn id_like_does_not_admit_untested_derivatives() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rocky = write_os_release(
        dir.path(),
        "ID=rocky\nID_LIKE=\"rhel centos fedora\"\nVERSION_ID=\"9.3\"\n",
    );
    assert!(
        matches!(check_os_from(&rocky), Err(CheckError::UnsupportedOs { .. })),
        "una derivata che dichiara ID_LIKE=fedora non deve entrare dalla finestra"
    );
}

#[test]
fn a_supported_os_carries_its_family() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ubuntu = write_os_release(
        dir.path(),
        "ID=ubuntu\nVERSION_ID=\"24.04\"\nVERSION_CODENAME=noble\n",
    );
    assert_eq!(
        check_os_from(&ubuntu).expect("ubuntu ok").family,
        OsFamily::Debian
    );

    let debian = write_os_release(
        dir.path(),
        "ID=debian\nVERSION_ID=\"12\"\nVERSION_CODENAME=bookworm\n",
    );
    assert_eq!(
        check_os_from(&debian).expect("debian ok").family,
        OsFamily::Debian
    );
}

/// Fedora è **riconosciuta** ma rifiutata, e le due cose vanno dette insieme.
///
/// Accettarla prima che esistano backend dnf, lista pacchetti e init del cluster
/// produrrebbe un'installazione che si ferma a metà: cioè lo stato intermedio
/// sporco che questo progetto esiste per non lasciare.
#[test]
fn fedora_is_recognised_but_refused_with_its_own_message() {
    let info = OsInfo {
        id: "fedora".to_string(),
        version: "41".to_string(),
        codename: None,
        family: OsFamily::Fedora,
    };

    let err = validate_os(&info).expect_err("Fedora non è ancora supportata");
    assert!(
        matches!(err, CheckError::NotYetSupportedOs { .. }),
        "atteso NotYetSupportedOs, trovato {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("non è ancora attivo") || msg.contains("in lavorazione"),
        "il messaggio deve dire «non ancora», non «non supportato»: {msg}"
    );
}

// --- La persistenza: il manifesto porta la famiglia --------------------------

/// **Il test che presidia il punto più facile da sbagliare.**
///
/// `to_context` costruisce il resto del `Context` con `..Default::default()`.
/// Se `os_family` cadesse lì dentro, ogni rollback lavorerebbe come `Debian` —
/// anche quello di un'installazione Fedora — e nessun test che non guardi
/// *questo* campo se ne accorgerebbe.
#[test]
fn to_context_propagates_the_recorded_family_not_the_default() {
    let ctx = config_for(OsFamily::Fedora).to_context(false, false, PathBuf::from("/tmp/s.json"));

    assert_eq!(
        ctx.os_family,
        OsFamily::Fedora,
        "la famiglia del rollback si legge dal manifesto: se qui compare il default \
         Debian, `to_context` ha smesso di propagarla e l'undo userebbe i comandi \
         sbagliati in silenzio"
    );
}

#[test]
fn from_context_records_the_family() {
    let ctx = Context {
        os_family: OsFamily::Fedora,
        ..Default::default()
    };
    assert_eq!(
        InstallConfig::from_context(&ctx).os_family,
        OsFamily::Fedora
    );
}

/// Andata e ritorno su disco: ciò che si scrive è ciò che si rilegge.
#[test]
fn the_family_survives_a_round_trip_through_the_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");

    let mut state = InstallState::default();
    state.set_config(config_for(OsFamily::Fedora));
    state.record(StepRecord {
        name: "prepare-opt-root".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });
    state.save(&path).expect("save");

    let riletto = InstallState::load(&path).expect("load");
    assert_eq!(
        riletto.config.expect("config presente").os_family,
        OsFamily::Fedora
    );
}

/// **Retrocompatibilità.** Un manifesto scritto prima che il campo esistesse non
/// lo dichiara, e va letto come `Debian` — che è la verità, perché ogni
/// installazione precedente è apt.
///
/// Il fixture è il formato reale, non una ricostruzione a memoria: rendere
/// illeggibile un manifesto significa rendere **non disinstallabile** un'istanza
/// già in campo, che è il danno di A-V3-1 per un'altra strada.
#[test]
fn a_manifest_written_before_this_field_reads_as_debian() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");

    let legacy = r#"{
  "completed": [
    { "name": "prepare-opt-root", "snapshot": "CreatedByUs" }
  ],
  "finished": false,
  "config": {
    "odoo_version": "18.0",
    "odoo_version_short": "18",
    "odoo_user": "odoo",
    "db_user": "odoo",
    "db_name": "citest",
    "odoo_home": "/opt/odoo",
    "install_dir": "/opt/odoo/odoo18",
    "port": 8069,
    "odoo_logfile": null,
    "with_nginx": false,
    "sudo_user": "omisen"
  }
}"#;
    std::fs::write(&path, legacy).expect("write");

    let state = InstallState::load(&path).expect("un manifesto pre-2.3 deve restare leggibile");
    let config = state.config.expect("config presente");
    assert_eq!(
        config.os_family,
        OsFamily::Debian,
        "senza il campo, la famiglia è Debian: è ciò che quell'installazione era"
    );
    assert_eq!(config.db_name, "citest", "il resto si legge come prima");
}

// --- L'identità: riprendere con un'altra famiglia non è riprendere -----------

/// La famiglia non *nomina* un artefatto, ma cambia il **significato** dei nomi
/// registrati: un delta scritto da apt non è riprendibile da dnf. Sta quindi in
/// `identity()`, e il rifiuto dice **quale** campo non coincide.
#[test]
fn resuming_with_a_different_family_is_refused_by_name() {
    let mut state = InstallState::default();
    state.set_config(config_for(OsFamily::Debian));
    state.record(StepRecord {
        name: "prepare-opt-root".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });

    let decision = start_decision(&state, &config_for(OsFamily::Fedora), false);

    match decision {
        StartDecision::RefuseIdentityMismatch(differenze) => {
            assert!(
                differenze
                    .iter()
                    .any(|(campo, _, _)| *campo == "famiglia OS"),
                "il rifiuto deve nominare il campo che non coincide, trovato: {differenze:?}"
            );
        }
        altro => panic!("atteso un rifiuto per identità diversa, trovato {altro:?}"),
    }
}

/// …e con la **stessa** famiglia si riprende, come prima. Un manifesto pre-2.3
/// (che si legge come `Debian`) su una macchina Debian deve restare riprendibile:
/// il campo nuovo non deve rompere il resume delle installazioni già in corso.
#[test]
fn resuming_on_the_same_family_still_works() {
    let mut state = InstallState::default();
    state.set_config(config_for(OsFamily::Debian));
    state.record(StepRecord {
        name: "prepare-opt-root".to_string(),
        snapshot: serde_json::to_value(PreState::CreatedByUs).expect("serialize"),
    });

    assert_eq!(
        start_decision(&state, &config_for(OsFamily::Debian), false),
        StartDecision::Resume
    );
}

// --- La discordanza: si avvisa, non si decide --------------------------------

/// Il sistema si legge per **avvisare**, mai per agire. Rifiutare renderebbe non
/// disinstallabile un'istanza; dedurre la famiglia dal sistema violerebbe la
/// regola per cui questo campo esiste.
#[test]
fn a_family_mismatch_warns_and_does_not_decide() {
    // Concordi → nessun avviso.
    assert!(family_mismatch(OsFamily::Debian, Some(OsFamily::Debian)).is_none());
    assert!(family_mismatch(OsFamily::Fedora, Some(OsFamily::Fedora)).is_none());

    // Sistema non identificabile → nessun avviso: non sappiamo abbastanza per
    // dire che c'è una discordanza.
    assert!(family_mismatch(OsFamily::Debian, None).is_none());

    // Discordi → avviso che nomina **entrambe** e dichiara con quale si procede.
    let avviso = family_mismatch(OsFamily::Debian, Some(OsFamily::Fedora))
        .expect("una discordanza va detta");
    assert!(
        avviso.contains("debian"),
        "deve nominare il manifesto: {avviso}"
    );
    assert!(
        avviso.contains("fedora"),
        "deve nominare il sistema: {avviso}"
    );
    assert!(
        avviso.contains("Procedo con 'debian'"),
        "deve dire che vince il manifesto, non il sistema: {avviso}"
    );
}

/// L'`ID` per l'avviso si legge **senza validare**: il rollback deve funzionare
/// anche su un sistema su cui rifiuteremmo di installare. Disinstallare
/// un'istanza non richiede che la macchina sia ancora adatta a ospitarla.
#[test]
fn the_id_for_the_warning_is_read_without_validating() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Una release troppo vecchia per installarci: `check_os_from` la rifiuta…
    let vecchia = write_os_release(dir.path(), "ID=ubuntu\nVERSION_ID=\"18.04\"\n");
    assert!(check_os_from(&vecchia).is_err());
    // …ma l'ID si legge lo stesso, ed è ciò che serve all'avviso.
    assert_eq!(os_id_from(&vecchia).as_deref(), Some("ubuntu"));

    // File assente → nessuna risposta, e quindi nessun avviso (vedi sopra).
    assert_eq!(os_id_from(&dir.path().join("assente")), None);
}
