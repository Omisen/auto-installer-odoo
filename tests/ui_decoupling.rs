//! Test strutturale (Fase 11): la UI è disaccoppiata.
//!
//! Gli step NON importano `inquire`/`indicatif`; il motore non importa
//! `indicatif` (dipende solo dall'astrazione `ProgressReporter`).

use std::fs;
use std::path::Path;

fn read_rs_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.extension().map(|e| e == "rs").unwrap_or(false) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let content = fs::read_to_string(&path).expect("read");
            out.push((name, content));
        }
    }
    out
}

/// `true` se il file **importa o usa** la crate (non un semplice riferimento in
/// un commento). Cerca `use <crate>` o `<crate>::`.
fn imports_or_uses(content: &str, krate: &str) -> bool {
    content.contains(&format!("use {krate}")) || content.contains(&format!("{krate}::"))
}

#[test]
fn steps_do_not_import_ui_crates() {
    let root = env!("CARGO_MANIFEST_DIR");
    let steps_dir = Path::new(root).join("src").join("steps");
    for (name, content) in read_rs_files(&steps_dir) {
        assert!(
            !imports_or_uses(&content, "inquire"),
            "{name} non deve dipendere da inquire"
        );
        assert!(
            !imports_or_uses(&content, "indicatif"),
            "{name} non deve dipendere da indicatif"
        );
    }
}

#[test]
fn engine_does_not_depend_on_indicatif() {
    let root = env!("CARGO_MANIFEST_DIR");
    let engine = Path::new(root).join("src").join("engine.rs");
    let content = fs::read_to_string(&engine).expect("read engine.rs");
    assert!(
        !imports_or_uses(&content, "indicatif"),
        "il motore deve dipendere dall'astrazione ProgressReporter, non da indicatif"
    );
    // Ma deve usare l'astrazione.
    assert!(
        content.contains("ProgressReporter"),
        "il motore usa l'astrazione di progresso"
    );
}
