#!/usr/bin/env bash
# Lettura del DIARIO dell'esecuzione dall'output dell'installer.
#
# Perché esiste un file a parte. Queste funzioni interpretano un formato — quello
# di `tracing` — e un formato si sbaglia in silenzio: un pattern che non combacia
# non produce un errore, produce ZERO RISULTATI. E zero risultati, in un test di
# pulizia, si presenta come «tutti i pacchetti del delta sono stati purgati» ✔
# quando in realtà non se n'è verificato nemmeno uno. Stando qui, la logica è
# esercitabile da `selftest-journal.sh` contro un campione fedele, nella CI
# veloce, senza aspettare un'installazione reale.
#
# ## Diario ≠ manifesto
#
# Il manifesto (`state.json`) dice cosa c'è ANCORA sul sistema: quando un
# rollback annulla uno step, quel record sparisce. Va benissimo per «cosa resta»,
# ed è inutilizzabile per «cosa è stato fatto» — in `MODE=probe` l'installazione
# si annulla da sé e il manifesto è correttamente vuoto. Il diario di ciò che è
# accaduto vive nel log, che non viene riscritto.
#
# ## Le due trappole del formato, entrambe pagate in campo
#
# 1. **Gli ANSI ci sono anche su pipe.** `fmt::layer()` di `tracing-subscriber`
#    non fa auto-detect del terminale: colora sempre. In un file catturato
#    «progress:» è in realtà «progress\e[0m\e[2m:\e[0m», e nessun pattern scritto
#    guardando ciò che si LEGGE a schermo combacia. GitHub rende gli escape
#    invisibili nei suoi log, quindi il difetto è invisibile due volte.
# 2. **`pacchetti=` non ha virgolette.** È un campo `Display` (`%`), non `Debug`:
#    il valore non è quotato e arriva fino a fine riga. `step="..."` invece è
#    quotato, perché è una stringa. Nella stessa riga convivono le due forme.

# Toglie i codici ANSI. Da applicare SEMPRE prima di qualunque altra lettura.
journal_strip_ansi() {
  sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$1"
}

# Gli step portati a termine, uno per riga: dalle righe di progresso «✔ <nome>».
journal_steps() {
  sed -n 's/.*progress: ✔ \([a-z0-9-]*\).*/\1/p' "$1" | sort -u
}

# I pacchetti nominati da uno step, uno per riga.
#
# `$2` è il messaggio che identifica la lista ("pacchetti aggiunti da noi" o
# "pacchetti già presenti, mai toccati"), `$3` il nome dello step.
#
# Lo step si passa e non si indovina: il delta di `bootstrap-prerequisites`
# (git/curl/wget/gettext) resta installato DI PROPOSITO, e confonderlo con quello
# di `install-system-dependencies` farebbe fallire la verifica di purga proprio
# sulle immagini minimali — dove quelle utility non sono preinstallate e quindi
# finiscono davvero nel delta.
#
# Un risultato VUOTO è legittimo (in `MODE=probe` lo step può non essere stato
# raggiunto) e non deve essere un errore: il `grep` che scarta le righe vuote
# esce 1 quando non trova nulla, e sotto `set -o pipefail` farebbe abortire lo
# script chiamante. Il `|| true` è quindi parte del contratto, non pigrizia.
journal_packages() {
  sed -n "s/.*delta pacchetti: $2 step=\"$3\" pacchetti=//p" "$1" \
    | tr ' ' '\n' | grep -v '^$' | sort -u || true
}
