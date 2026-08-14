//! M2: the two catalogues must **correspond**, not merely be frozen.
//!
//! the earlier lesson was "freeze the list, so a refactor that loses a package
//! says so at once". with two families it extends: if the lists live in two
//! files, adding a dependency to only one compiles fine, and the gap surfaces
//! when a VM stops building — the most expensive place possible.
//!
//! this is why `DepId` exists. resolution still works on names; the identifier
//! makes the correspondence **checkable**.
//!
//! what this cannot say is whether the translated names are **right**. that
//! check is a dry run on a real VM, which resolves every group without mutating
//! and reports the missing ones in a single message. here only the
//! **structure** is verified — that no need is uncovered — which is all a mock
//! test can guarantee.

use invok::packaging::apt::AptBackend;
use invok::packaging::dnf::DnfBackend;
use invok::packaging::{DepId, PackageManager};

/// every declared need is covered by **both** families.
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

/// no catalogue entry is **empty**.
///
/// an entry with no packages would pass a naive "the need is listed" check
/// while installing nothing — claiming to cover a need without covering it is
/// worse than omitting it, because it removes the suspicion too.
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

/// a need may cost **several packages** on one family and one on the other: the
/// build toolchain has no metapackage everywhere.
///
/// the test pins that the correspondence is **not** one-to-one — tightening it
/// would break the very case that motivated the structure.
#[test]
fn a_need_may_cost_more_packages_on_one_family() {
    let debian = AptBackend.catalog();
    let fedora = DnfBackend.catalog();

    let voce = |catalog: &invok::packaging::PackageCatalog, id: DepId| {
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

/// the **same** package may satisfy two needs: on one family two identifiers
/// collapse onto a single name.
///
/// exactly A-MD-1, and there it is the norm rather than an edge case:
/// deduplicating resolved names is what keeps the delta honest.
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

/// the PostgreSQL **server** packages differ, and so does the marker.
///
/// on one family the bare name is the client only: installing just that gives a
/// start that fails without saying why, and using it as the marker would call a
/// non-existent server `Preexisting` — no stop, no undo, a cluster left
/// running.
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

    // nginx is named the same on both: not everything diverges, and pretending
    // otherwise would add a translation to maintain for nothing.
    assert_eq!(debian.nginx, fedora.nginx);
}

/// the two catalogues' names **diverge**: copying one into the other
/// untranslated would still pass the coverage test above — every need
/// "covered", by a name that does not exist there.
///
/// this does not check which names are right (only a VM can say), but that a
/// translation happened at all.
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

    // the non-mechanical cases, pinned so a future "cleanup" does not reduce
    // them to a naive suffix swap.
    for atteso in [
        "openldap-devel",      // libldap2-dev
        "cyrus-sasl-devel",    // libsasl2-dev
        "libjpeg-turbo-devel", // libjpeg-dev
        "libxslt-devel",       // libxslt1-dev: cade l'1
        // the zlib case is two steps, not one: that release migrated to a
        // different implementation and the obvious name is only a `Provides`.
        // the second step shows up on a real machine only.
        "zlib-ng-compat-devel",
    ] {
        assert!(
            fedora.iter().any(|n| n == atteso),
            "'{atteso}' non è nella lista Fedora: è una delle traduzioni che NON \
             si ottengono sostituendo -dev con -devel"
        );
    }
}

// --- the commands: what a real machine cannot disprove in time --------------

/// **the ratified point of the second fork.**
///
/// one manager removes orphaned dependencies by default: exactly the global
/// autoremove that R0 **banned** from the undo, because it is not bounded by
/// our delta. there it would happen in **every** rollback, and could take away
/// a library shared with the customer's software.
///
/// the flag is always passed, even if the default changed one day: a behaviour
/// a promise rests on is not left to a configuration file we do not control.
#[test]
fn dnf_remove_never_touches_orphaned_dependencies() {
    let args = invok::packaging::dnf::remove_args(&["pippo", "pluto"]);

    assert!(
        args.iter()
            .any(|a| a == "--setopt=clean_requirements_on_remove=False"),
        "senza questo flag il rollback su Fedora rimuove più di quanto ha messo: {args:?}"
    );
    assert_eq!(args.first().map(String::as_str), Some("remove"));
    assert!(args.iter().any(|a| a == "pippo") && args.iter().any(|a| a == "pluto"));
}

/// **weak** dependencies stay out of the delta.
///
/// the counterpart of the recommends switch: without it the manager pulls in
/// suggestions that land in the delta and that the undo would then remove —
/// packages nobody asked for, taken from somebody who did not ask for them.
#[test]
fn neither_family_installs_weak_dependencies() {
    let dnf = invok::packaging::dnf::install_args(&["pippo"]);
    assert!(
        dnf.iter().any(|a| a == "--setopt=install_weak_deps=False"),
        "dnf: {dnf:?}"
    );

    let apt = invok::packaging::apt::install_args(&["pippo"]);
    assert!(
        apt.iter().any(|a| a == "--no-install-recommends"),
        "apt: {apt:?}"
    );
}

/// **verified in the field:** the newer manager rejects the `--` separator that
/// is half the double defence against argument injection (R1).
///
/// it answers `Unknown argument "--"` and exits non-zero: adding it hardens
/// nothing and **breaks the command**. on that family only the validator
/// remains, and the real surface is nil — the names come from the catalogue,
/// which is made of constants in the source.
///
/// the test exists because the urge to "realign the two families" is concrete,
/// and the symptom would be an installation failing on its first package.
#[test]
fn dnf_does_not_use_the_argument_separator() {
    for args in [
        invok::packaging::dnf::install_args(&["pippo"]),
        invok::packaging::dnf::remove_args(&["pippo"]),
    ] {
        assert!(
            !args.iter().any(|a| a == "--"),
            "dnf5 rifiuta `--`: metterlo rompe il comando invece di proteggerlo ({args:?})"
        );
    }

    // the query tool does accept the separator, and there the double defence
    // holds.
}

/// the families use different verbs, and that is fine: what must coincide is
/// the **promise**, not the command.
#[test]
fn the_two_families_speak_different_commands() {
    let apt = invok::packaging::apt::remove_args(&["pippo"]);
    let dnf = invok::packaging::dnf::remove_args(&["pippo"]);

    assert_eq!(apt.first().map(String::as_str), Some("purge"));
    assert_eq!(dnf.first().map(String::as_str), Some("remove"));
    assert!(
        !dnf.iter().any(|a| a == "purge"),
        "«purge» è un concetto deb: su rpm non esiste, e prometterlo sarebbe falso"
    );
}

/// the token comparison holds on the other firewall too: A-V3-7 on the second
/// family, before it can happen.
#[test]
fn firewalld_does_not_find_port_80_inside_port_8080() {
    use invok::distro::firewalld::port_in_list;

    assert!(!port_in_list("8080/tcp 443/tcp", "80/tcp"));
    assert!(port_in_list("8080/tcp 80/tcp 53/udp", "80/tcp"));
    assert!(port_in_list("80/tcp", "80/tcp"));
    assert!(!port_in_list("", "80/tcp"));
}

/// the diagnostic hint names **the family's** command.
///
/// naming the wrong one is worse than no hint: it sends the reader to a command
/// their machine does not have and casts doubt on the rest of the diagnosis —
/// exactly when the installation has stopped and they are trying to understand
/// why.
#[test]
fn the_refresh_hint_names_the_right_command() {
    assert_eq!(AptBackend.refresh_command(), "apt-get update");
    assert_eq!(DnfBackend.refresh_command(), "dnf makecache");
}

/// **the three virtual names found in the field.**
///
/// they are not packages on that release but `Provides` of others. a virtual
/// name is installable, yet the query tool does not know it, so removal exits
/// zero having removed nothing: the delta would list it, the report would say
/// "removed", and the package would stay. an **invisible** leftover — the worst
/// kind, and A5.1-bis in its rpm form.
///
/// the test freezes the **real** name in each group. without it a future
/// "cleanup" could reduce them to the canonical name and silently reopen the
/// defect: the installation would keep working, and only the rollback would
/// lie.
#[test]
fn the_fedora_list_declares_the_real_name_for_each_virtual_one() {
    let specs = DnfBackend.catalog();
    let gruppo = |virtuale: &str| -> Vec<String> {
        specs
            .bootstrap_specs()
            .into_iter()
            .chain(specs.odoo_specs())
            .find(|s| s.alternatives().iter().any(|n| n == virtuale))
            .unwrap_or_else(|| panic!("'{virtuale}' deve restare in lista"))
            .alternatives()
            .to_vec()
    };

    for (virtuale, reale) in [
        ("wget", "wget1-wget"),
        ("zlib-devel", "zlib-ng-compat-devel"),
        ("openjpeg2-devel", "openjpeg-devel"),
    ] {
        let alternative = gruppo(virtuale);
        assert!(
            alternative.iter().any(|n| n == reale),
            "'{virtuale}' è virtuale su Fedora 41: il gruppo deve offrire il nome \
             reale '{reale}', trovato {alternative:?}"
        );
        assert_eq!(
            alternative.first().map(String::as_str),
            Some(reale),
            "il nome reale va per PRIMO: è quello che compare nei messaggi \
             diagnostici, e quello che vogliamo nel delta"
        );
    }
}
