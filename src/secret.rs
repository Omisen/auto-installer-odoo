//! [`Secret`]: stringa sensibile che non trapela mai nei log.
//!
//! La password admin non deve finire nei log né in output di `Debug` (regola di
//! `CLAUDE.md` e del prompt di Fase 1). `Secret` avvolge la stringa e ne
//! ridefinisce `Debug` in modo che stampi solo un placeholder; il valore reale
//! è accessibile solo tramite [`Secret::expose`], usato esclusivamente quando
//! il valore va scritto dove serve davvero (es. il file di config di Odoo).

use std::fmt;

/// Stringa sensibile con `Debug` redatto. Non implementa `Display` di proposito.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Avvolge un valore sensibile.
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Espone il valore in chiaro. Usare solo dove il segreto serve davvero,
    /// mai in un log.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// `true` se il valore è vuoto.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(****)")
    }
}
