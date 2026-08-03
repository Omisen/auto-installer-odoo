//! M2 — le due liste devono **corrispondersi**, non solo essere congelate.
//!
//! La lezione di R6-hotfix-2 era «congela la lista, così un refactor che perde
//! un pacchetto lo dice subito». Con due famiglie la lezione si estende: se le
//! due liste vivono in due file, aggiungere una dipendenza a una sola compila
//! benissimo, e la mancanza si scopre quando una VM Fedora non compila più —
//! cioè nel posto più caro possibile.
//!
//! È il test per cui `DepId` esiste. Non serve alla risoluzione, che continua a
//! lavorare su nomi: serve a rendere la corrispondenza **verificabile**.
//!
//! # Cosa questo test NON può dire
//!
//! Che i nomi Fedora siano **giusti**. Sono la traduzione della lista Debian e
//! nessuno li ha ancora provati su una macchina vera: quel controllo lo fa
//! `sudo odoo-installer --dry-run` su una VM Fedora, che risolve tutti i gruppi
//! senza mutare nulla e riporta in un solo messaggio quelli che non esistono.
//! Qui si verifica la **struttura** — che nessun bisogno sia scoperto — che è
//! l'unica cosa che un test su mock può garantire.

use odoo_installer::packaging::apt::AptBackend;
use odoo_installer::packaging::dnf::DnfBackend;
use odoo_installer::packaging::{DepId, PackageManager};

/// Ogni bisogno dichiarato è coperto da **entrambe** le famiglie.
#[test]
fn every_need_is_covered_by_every_family() {
    let cataloghi = [
        ("debian", AptBackend.catalog()),
        ("fedora", DnfBackend.catalog()),
    ];

    for (famiglia, catalog) in &cataloghi {
        let scoperti: Vec<&DepId> = DepId::ALL
            .iter()
            .filter(|id| !catalog.covers(**id))
            .collect();

        assert!(
            scoperti.is_empty(),
            "la famiglia '{famiglia}' non copre questi bisogni: {scoperti:?}. \
             Aggiungere una dipendenza a una sola famiglia compila benissimo: è \
             questo il test che lo impedisce"
        );
    }
}

/// Nessuna voce di catalogo è **vuota**.
///
/// Una voce senza pacchetti passerebbe un controllo ingenuo di «il bisogno è
/// elencato» pur non installando nulla — dichiarare di coprire un bisogno senza
/// coprirlo è peggio che ometterlo, perché toglie anche il sospetto.
#[test]
fn no_catalog_entry_is_empty() {
    for (famiglia, catalog) in [
        ("debian", AptBackend.catalog()),
        ("fedora", DnfBackend.catalog()),
    ] {
        for entry in catalog.bootstrap.iter().chain(catalog.odoo.iter()) {
            assert!(
                !entry.specs.is_empty(),
                "{famiglia}: la voce {:?} non elenca alcun pacchetto",
                entry.id
            );
            for spec in &entry.specs {
                assert!(
                    !spec.alternatives().is_empty(),
                    "{famiglia}: la voce {:?} ha un gruppo di alternative vuoto",
                    entry.id
                );
            }
        }
    }
}

/// Un bisogno può costare **più pacchetti** su una famiglia e uno sull'altra: è
/// il caso di `build-essential`, che su Fedora non esiste come metapacchetto.
///
/// Il test serve a fissare che la corrispondenza **non è 1:1** — chi la
/// irrigidisse in «un bisogno, un pacchetto» romperebbe proprio il caso che ha
/// motivato la struttura.
#[test]
fn a_need_may_cost_more_packages_on_one_family() {
    let debian = AptBackend.catalog();
    let fedora = DnfBackend.catalog();

    let voce = |catalog: &odoo_installer::packaging::PackageCatalog, id: DepId| {
        catalog
            .odoo
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.specs.len())
            .unwrap_or(0)
    };

    assert_eq!(
        voce(&debian, DepId::BuildTools),
        1,
        "su Debian è il metapacchetto build-essential"
    );
    assert!(
        voce(&fedora, DepId::BuildTools) >= 3,
        "su Fedora servono almeno gcc, gcc-c++ e make: `@development-tools` è un \
         gruppo dnf, con una sintassi propria e un comportamento poco chiaro alla \
         rimozione — il delta non saprebbe cosa reclamare"
    );
}

/// Lo **stesso** pacchetto può soddisfare due bisogni: su Fedora `Jpeg` e
/// `Jpeg8` collassano entrambi su `libjpeg-turbo-devel`.
///
/// È esattamente A-MD-1, e su questa famiglia non è un caso di bordo ma la
/// norma: la deduplica dei nomi risolti è ciò che tiene onesto il delta.
#[test]
fn two_needs_may_share_one_package_on_fedora() {
    let fedora = DnfBackend.catalog();
    let nome = |id: DepId| {
        fedora
            .odoo
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.specs.first())
            .map(|s| s.preferred().to_string())
            .expect("la voce esiste")
    };

    assert_eq!(
        nome(DepId::Jpeg),
        nome(DepId::Jpeg8),
        "i due bisogni jpeg cadono sullo stesso pacchetto: è la deduplica di \
         A-MD-1 a impedire che il delta lo elenchi due volte"
    );
}

/// I pacchetti del **server** PostgreSQL non sono gli stessi, e il marker nemmeno.
///
/// Su Fedora `postgresql` è il solo client: installarlo soltanto porterebbe a un
/// `systemctl start postgresql` che fallisce senza dire perché, e usarlo come
/// marker farebbe risultare `Preexisting` un server che non c'è — quindi nessuno
/// stop, nessun undo, e un cluster lasciato in piedi.
#[test]
fn the_postgres_server_is_a_different_package_on_each_family() {
    let debian = AptBackend.catalog();
    let fedora = DnfBackend.catalog();

    assert!(debian.postgres.contains(&"postgresql".to_string()));
    assert!(
        fedora.postgres.contains(&"postgresql-server".to_string()),
        "su Fedora il server è un pacchetto a parte dal client"
    );
    assert_eq!(debian.postgres_marker, "postgresql");
    assert_eq!(fedora.postgres_marker, "postgresql-server");

    // nginx invece si chiama uguale su entrambe: non tutto diverge, e fingere il
    // contrario aggiungerebbe una traduzione da mantenere per niente.
    assert_eq!(debian.nginx, fedora.nginx);
}

/// I nomi Fedora **divergono** da quelli Debian: se qualcuno copiasse la lista
/// Debian in `dnf.rs` senza tradurla, il test di copertura sopra passerebbe lo
/// stesso — ogni bisogno sarebbe «coperto», da un nome che su Fedora non esiste.
///
/// Non si verifica quali siano i nomi giusti (solo una VM può dirlo), ma che una
/// traduzione ci sia stata.
#[test]
fn the_fedora_names_are_not_the_debian_ones() {
    let debian: Vec<String> = AptBackend
        .catalog()
        .odoo_specs()
        .iter()
        .map(|s| s.preferred().to_string())
        .collect();
    let fedora: Vec<String> = DnfBackend
        .catalog()
        .odoo_specs()
        .iter()
        .map(|s| s.preferred().to_string())
        .collect();

    let comuni = fedora.iter().filter(|n| debian.contains(n)).count();
    assert!(
        comuni < fedora.len() / 2,
        "troppi nomi identici fra le due famiglie ({comuni} su {}): la lista Fedora \
         sembra una copia non tradotta di quella Debian",
        fedora.len()
    );

    // I casi che divergono in modo non meccanico, fissati perché una futura
    // "pulizia" non li riporti a una traduzione ingenua `-dev` → `-devel`.
    for atteso in [
        "openldap-devel",      // libldap2-dev
        "cyrus-sasl-devel",    // libsasl2-dev
        "libjpeg-turbo-devel", // libjpeg-dev
        "zlib-devel",          // zlib1g-dev: cade il soname
        "libxslt-devel",       // libxslt1-dev: cade l'1
    ] {
        assert!(
            fedora.iter().any(|n| n == atteso),
            "'{atteso}' non è nella lista Fedora: è una delle traduzioni che NON \
             si ottengono sostituendo -dev con -devel"
        );
    }
}

// --- I comandi: ciò che una macchina reale non può smentire in tempo --------

/// **Il punto ratificato del Bivio 2.**
///
/// Il default di `dnf remove` è rimuovere anche le dipendenze diventate orfane:
/// sarebbe esattamente l'`apt-get autoremove` globale che R0 ha **bandito**
/// dall'undo perché non è delimitato dal nostro delta. Su apt quella rimozione è
/// un'azione esplicita, confinata a `--aggressive-rollback`; su dnf accadrebbe
/// in **ogni** rollback, e potrebbe portarsi via una libreria condivisa con
/// software del cliente.
///
/// Il flag si passa sempre, anche se il default un giorno cambiasse: un
/// comportamento su cui poggia una promessa non si lascia decidere a un file di
/// configurazione che non controlliamo.
#[test]
fn dnf_remove_never_touches_orphaned_dependencies() {
    let args = odoo_installer::packaging::dnf::remove_args(&["pippo", "pluto"]);

    assert!(
        args.iter()
            .any(|a| a == "--setopt=clean_requirements_on_remove=False"),
        "senza questo flag il rollback su Fedora rimuove più di quanto ha messo: {args:?}"
    );
    assert_eq!(args.first().map(String::as_str), Some("remove"));
    assert!(
        args.iter().any(|a| a == "--"),
        "il separatore protegge dai nomi che iniziano con '-' (stessa rete di R1)"
    );
    assert!(args.iter().any(|a| a == "pippo") && args.iter().any(|a| a == "pluto"));
}

/// Le dipendenze **deboli** non entrano nel delta.
///
/// `install_weak_deps=False` è la controparte di `--no-install-recommends`:
/// senza, il gestore tira dentro i `Recommends`, che finiscono nel delta e che
/// l'undo poi rimuoverebbe — pacchetti che nessuno ha chiesto, tolti a qualcuno
/// che non li aveva chiesti.
#[test]
fn neither_family_installs_weak_dependencies() {
    let dnf = odoo_installer::packaging::dnf::install_args(&["pippo"]);
    assert!(
        dnf.iter().any(|a| a == "--setopt=install_weak_deps=False"),
        "dnf: {dnf:?}"
    );

    let apt = odoo_installer::packaging::apt::install_args(&["pippo"]);
    assert!(
        apt.iter().any(|a| a == "--no-install-recommends"),
        "apt: {apt:?}"
    );
}

/// Le due famiglie usano verbi diversi, e va bene: ciò che deve coincidere è la
/// **promessa**, non il comando.
#[test]
fn the_two_families_speak_different_commands() {
    let apt = odoo_installer::packaging::apt::remove_args(&["pippo"]);
    let dnf = odoo_installer::packaging::dnf::remove_args(&["pippo"]);

    assert_eq!(apt.first().map(String::as_str), Some("purge"));
    assert_eq!(dnf.first().map(String::as_str), Some("remove"));
    assert!(
        !dnf.iter().any(|a| a == "purge"),
        "«purge» è un concetto deb: su rpm non esiste, e prometterlo sarebbe falso"
    );
}

/// Il confronto per token vale anche su firewalld: `80/tcp` **non** è dentro
/// `8080/tcp`. È A-V3-7 sulla seconda famiglia, prima che possa succedere.
#[test]
fn firewalld_does_not_find_port_80_inside_port_8080() {
    use odoo_installer::distro::firewalld::port_in_list;

    assert!(!port_in_list("8080/tcp 443/tcp", "80/tcp"));
    assert!(port_in_list("8080/tcp 80/tcp 53/udp", "80/tcp"));
    assert!(port_in_list("80/tcp", "80/tcp"));
    assert!(!port_in_list("", "80/tcp"));
}
