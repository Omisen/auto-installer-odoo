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

    for (family, catalog) in &cataloghi {
        let uncovered: Vec<&DepId> = DepId::ALL
            .iter()
            .filter(|id| !catalog.covers(**id))
            .collect();

        assert!(
            uncovered.is_empty(),
            "family '{family}' does not cover these needs: {uncovered:?}. adding a dependency \
             to a single family compiles fine: this is the test that stops it"
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
    for (family, catalog) in [
        ("debian", AptBackend.catalog()),
        ("fedora", DnfBackend.catalog()),
    ] {
        for entry in catalog.bootstrap.iter().chain(catalog.odoo.iter()) {
            assert!(
                !entry.specs.is_empty(),
                "{family}: entry {:?} lists no package",
                entry.id
            );
            for spec in &entry.specs {
                assert!(
                    !spec.alternatives().is_empty(),
                    "{family}: entry {:?} has an empty alternatives group",
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
        "on one family it is the build-essential metapackage"
    );
    assert!(
        voce(&fedora, DepId::BuildTools) >= 3,
        "on the other at least gcc, gcc-c++ and make are needed: the group form has its \
         own syntax and unclear removal behaviour — the delta would not know what to \
         reclaim"
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
    let name = |id: DepId| {
        fedora
            .odoo
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.specs.first())
            .map(|s| s.preferred().to_string())
            .expect("la voce esiste")
    };

    assert_eq!(
        name(DepId::Jpeg),
        name(DepId::Jpeg8),
        "the two jpeg needs fall on the same package: A-MD-1's deduplication is what \
         stops the delta listing it twice"
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
        "there the server is a separate package from the client"
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

    let common = fedora.iter().filter(|n| debian.contains(n)).count();
    assert!(
        common < fedora.len() / 2,
        "too many identical names between the two families ({common} of {}): one \
         catalogue looks like an untranslated copy of the other",
        fedora.len()
    );

    // the non-mechanical cases, pinned so a future "cleanup" does not reduce
    // them to a naive suffix swap.
    for expected in [
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
            fedora.iter().any(|n| n == expected),
            "'{expected}' is not in that catalogue: one of the translations you do NOT get by \
             swapping the suffix"
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
        "without this flag the rollback removes more than it added: {args:?}"
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
            "the newer manager rejects `--`: adding it breaks the command instead of protecting it ({args:?})"
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
        "\"purge\" is one family's concept: it does not exist on the other, and promising it would be false"
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
    let group = |virtual_name: &str| -> Vec<String> {
        specs
            .bootstrap_specs()
            .into_iter()
            .chain(specs.odoo_specs())
            .find(|s| s.alternatives().iter().any(|n| n == virtual_name))
            .unwrap_or_else(|| panic!("'{virtual_name}' must stay in the list"))
            .alternatives()
            .to_vec()
    };

    for (virtual_name, real_name) in [
        ("wget", "wget1-wget"),
        ("zlib-devel", "zlib-ng-compat-devel"),
        ("openjpeg2-devel", "openjpeg-devel"),
    ] {
        let alternative = group(virtual_name);
        assert!(
            alternative.iter().any(|n| n == real_name),
            "'{virtual_name}' is virtual on that release: the group must offer the real name \
             '{real_name}', found {alternative:?}"
        );
        assert_eq!(
            alternative.first().map(String::as_str),
            Some(real_name),
            "the real name comes FIRST: it is the one that appears in the diagnostics, and \
             the one we want in the delta"
        );
    }
}
