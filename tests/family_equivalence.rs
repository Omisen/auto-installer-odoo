//! M2 — la garanzia che le protezioni critiche **non dipendono dalla famiglia**.
//!
//! Il design del supporto multi-distro promette che anti-drop del database,
//! hard-stop sull'init, cura del `.bashrc` e regola del filestore restino
//! intatte sotto entrambi i backend. Fino a M1 quella promessa era un'analisi:
//! quegli step non chiamano il gestore di pacchetti, quindi non possono
//! dipenderne. Con due famiglie vere diventa **verificabile**, ed è quello che
//! fa questo file.
//!
//! # Perché non basta l'analisi
//!
//! Perché niente impedisce a qualcuno, domani, di leggere `ctx.os_family` dentro
//! `create-database` «solo per un log» e poi di ramificarci sopra. Il difetto
//! non sarebbe visibile in nessun test degli step — che girano su una famiglia
//! sola — e comparirebbe in campo come «su Fedora il rollback non ha droppato il
//! database». Questo test fallisce prima.
//!
//! # Cosa si confronta
//!
//! La **sequenza di operazioni** che arriva al sistema e lo **snapshot
//! persistito**: sono le due cose da cui dipendono, rispettivamente, ciò che
//! viene fatto e ciò che l'undo potrà disfare.

mod common;

use common::{ops_of, MockConfig, MockSystemOps, Op};
use invok::context::Context;
use invok::distro::OsFamily;
use invok::step::Step;

fn ctx(family: OsFamily) -> Context {
    Context {
        dry_run: false,
        odoo_user: "odoo".to_string(),
        db_user: "odoo".to_string(),
        db_name: "citest".to_string(),
        odoo_home: "/opt/odoo".into(),
        install_dir: "/opt/odoo/odoo18".into(),
        odoo_version_short: "18".to_string(),
        os_family: family,
        ..Default::default()
    }
}

fn cfg(family: OsFamily) -> MockConfig {
    MockConfig {
        family,
        ..MockConfig::default()
    }
}

/// Sostituisce i suffissi **casuali** dei file temporanei con un segnaposto.
///
/// I temporanei hanno un nome imprevedibile per costruzione (A-V3-3/R9: un nome
/// fisso scritto nel sorgente è noto a chiunque), quindi due esecuzioni dello
/// stesso step differiscono sempre lì. Senza questa normalizzazione il confronto
/// fra famiglie fallirebbe per una ragione che non c'entra nulla — ed è successo
/// alla prima esecuzione di questo test, il che è anche la prova che il confronto
/// guarda davvero i valori e non solo la forma.
fn senza_suffissi_casuali(op: &Op) -> String {
    let reso = format!("{op:?}");
    let mut out = String::with_capacity(reso.len());
    let mut hex = String::new();

    let scarica = |out: &mut String, hex: &mut String| {
        // Il suffisso di `private_temp_path` è una corsa di cifre esadecimali
        // lunga e senza separatori: una parola normale non lo è.
        if hex.len() >= 12 {
            out.push_str("<rnd>");
        } else {
            out.push_str(hex);
        }
        hex.clear();
    };

    for c in reso.chars() {
        if c.is_ascii_hexdigit() {
            hex.push(c);
        } else {
            scarica(&mut out, &mut hex);
            out.push(c);
        }
    }
    scarica(&mut out, &mut hex);
    out
}

/// Esegue snapshot → run → undo di uno step, per una famiglia, e restituisce
/// ciò che il sistema ha visto e ciò che è stato persistito.
fn esegui<S: Step>(
    costruisci: impl Fn(Box<dyn invok::system_ops::SystemOps>) -> S,
    family: OsFamily,
) -> (Vec<String>, serde_json::Value) {
    let (ops, log) = MockSystemOps::new(cfg(family));
    let c = ctx(family);
    let mut step = costruisci(Box::new(ops));

    // Gli errori non si propagano: alcuni step falliscono per configurazione del
    // mock, e ciò che conta è che falliscano **allo stesso modo** su entrambe.
    let _ = step.snapshot(&c);
    let _ = step.run(&c);
    let _ = step.undo(&c);

    let ops: Vec<String> = ops_of(&log).iter().map(senza_suffissi_casuali).collect();
    (ops, step.snapshot_value())
}

/// Il cuore: uno step deve fare **le stesse cose** su entrambe le famiglie.
fn indifferente_alla_famiglia<S: Step>(
    nome: &str,
    costruisci: impl Fn(Box<dyn invok::system_ops::SystemOps>) -> S,
) {
    let (ops_debian, snap_debian) = esegui(&costruisci, OsFamily::Debian);
    let (ops_fedora, snap_fedora) = esegui(&costruisci, OsFamily::Fedora);

    assert_eq!(
        ops_debian, ops_fedora,
        "'{nome}' si comporta diversamente a seconda della famiglia: non deve. \
         Se questo step ha iniziato a leggere `ctx.os_family`, la protezione che \
         presidia ha smesso di essere distro-indipendente"
    );
    assert_eq!(
        snap_debian, snap_fedora,
        "'{nome}' persiste uno snapshot diverso a seconda della famiglia: l'undo \
         di domani vedrebbe due verità diverse per lo stesso artefatto"
    );
}

/// **Le quattro protezioni critiche**, più gli step che le circondano.
///
/// L'elenco è la parte che conta: ogni voce è uno step che il design dichiara
/// distro-indipendente. Aggiungere qui uno step nuovo è il modo di dichiarare
/// che anche lui lo è — e di scoprire subito se non lo è.
#[test]
fn the_critical_protections_do_not_depend_on_the_family() {
    use invok::steps::*;

    // anti-drop del database: il verdetto è `PreState`, il comando è `dropdb`.
    indifferente_alla_famiglia("create-database", |o| {
        create_database::CreateDatabase::with_ops(o)
    });
    // hard-stop sull'init: precondizione su `db_created_by_us`.
    indifferente_alla_famiglia("initialize-odoo-database", |o| {
        initialize_odoo_database::InitializeOdooDatabase::with_ops(o)
    });
    // cura del `.bashrc`: filesystem puro, backup e riga singola.
    indifferente_alla_famiglia("patch-bashrc", patch_bashrc::PatchBashrc::with_ops);
    // filestore: doppia condizione (creato da noi **e** database nostro).
    indifferente_alla_famiglia("setup-data-dir", |o| {
        setup_data_dir::SetupDataDir::with_ops(o)
    });

    // Il ruolo PostgreSQL e il resto del perimetro reversibile.
    indifferente_alla_famiglia("create-db-role", |o| {
        create_db_role::CreateDbRole::with_ops(o)
    });
    indifferente_alla_famiglia("prepare-opt-root", |o| {
        prepare_opt_root::PrepareOptRoot::with_ops(o)
    });
    indifferente_alla_famiglia("create-odoo-user", |o| {
        create_odoo_user::CreateOdooUser::with_ops(o)
    });
    indifferente_alla_famiglia("setup-cache-dir", |o| {
        setup_cache_dir::SetupCacheDir::with_ops(o)
    });
    indifferente_alla_famiglia("setup-log-dir", setup_log_dir::SetupLogDir::with_ops);
    indifferente_alla_famiglia("clone-odoo-repo", |o| {
        clone_odoo_repo::CloneOdooRepo::with_ops(o)
    });
    indifferente_alla_famiglia("create-virtualenv", |o| {
        create_virtualenv::CreateVirtualenv::with_ops(o)
    });
    indifferente_alla_famiglia("install-python-requirements", |o| {
        install_python_requirements::InstallPythonRequirements::with_ops(o)
    });
    indifferente_alla_famiglia("generate-config", |o| {
        generate_config::GenerateConfig::with_ops(o)
    });
    indifferente_alla_famiglia("setup-systemd", |o| {
        setup_systemd::SetupSystemd::with_ops(o)
    });
    indifferente_alla_famiglia("write-control-script", |o| {
        write_control_script::WriteControlScript::with_ops(o)
    });
}

/// Il contrappeso: gli step che **devono** dipendere dalla famiglia lo fanno
/// davvero.
///
/// Senza questo, il test sopra passerebbe anche se il supporto multi-distro non
/// funzionasse affatto — «tutti gli step si comportano uguale» è il risultato
/// che si otterrebbe se la famiglia non arrivasse mai a destinazione.
#[test]
fn the_packaging_steps_do_depend_on_the_family() {
    use invok::steps::apt_packages::AptPackagesStep;

    let (ops_debian, _) = esegui(
        AptPackagesStep::odoo_dependencies_with_ops,
        OsFamily::Debian,
    );
    let (ops_fedora, _) = esegui(
        AptPackagesStep::odoo_dependencies_with_ops,
        OsFamily::Fedora,
    );

    assert_ne!(
        ops_debian, ops_fedora,
        "install-system-dependencies deve installare pacchetti DIVERSI sulle due \
         famiglie: se sono uguali, la famiglia non è arrivata fino al catalogo"
    );

    let reso = |ops: &[String]| ops.join(" ");

    assert!(
        reso(&ops_debian).contains("build-essential"),
        "su Debian il compilatore arriva col metapacchetto"
    );
    assert!(
        reso(&ops_fedora).contains("gcc-c++"),
        "su Fedora servono i nomi espliciti: build-essential non esiste"
    );
    assert!(
        !reso(&ops_fedora).contains("build-essential"),
        "un nome Debian nella lista Fedora bloccherebbe l'installazione nello \
         snapshot — che è il comportamento giusto, ma qui vuol dire lista non tradotta"
    );
}

/// Su Fedora il delta **non** contiene doppioni, anche se due bisogni cadono
/// sullo stesso pacchetto (A-MD-1 nella sua forma strutturale).
#[test]
fn the_fedora_delta_has_no_duplicates() {
    use invok::steps::apt_packages::AptPackagesStep;

    let (ops, _) = esegui(
        AptPackagesStep::odoo_dependencies_with_ops,
        OsFamily::Fedora,
    );

    // I pacchetti passati a `install`, estratti dalla riga resa dell'operazione.
    let riga = ops
        .iter()
        .find(|o| o.starts_with("PkgInstall"))
        .cloned()
        .unwrap_or_default();
    let installati: Vec<String> = riga
        .trim_start_matches("PkgInstall([")
        .trim_end_matches("])")
        .split(", ")
        .map(|n| n.trim_matches('"').to_string())
        .filter(|n| !n.is_empty())
        .collect();

    let mut visti = std::collections::HashSet::new();
    let doppi: Vec<&String> = installati.iter().filter(|n| !visti.insert(*n)).collect();

    assert!(
        doppi.is_empty(),
        "sulla famiglia Fedora più bisogni cadono sullo stesso pacchetto \
         (i due jpeg): il delta deve elencarlo una volta sola, trovati {doppi:?}"
    );
}
