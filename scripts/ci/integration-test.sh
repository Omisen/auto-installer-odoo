#!/usr/bin/env bash
# =============================================================================
# scripts/ci/integration-test.sh — test di INTEGRAZIONE REALE (R5)
#
# Installa Odoo davvero, verifica che funzioni, poi esegue
# `odoo-installer rollback` e verifica che il sistema sia tornato pulito.
#
# È l'automazione della sessione di verifica manuale su Multipass che ha
# trovato A-RT-1 (dpkg -i non risolve le dipendenze → installazione impossibile
# su ogni sistema minimale) e A-RT-2 (il purge del rollback falliva su dpkg
# rotto → 24 pacchetti residui). Nessuno dei due era visibile ai test su mock:
# i mock modellano ciò che sappiamo del sistema, e quei due bug stavano
# esattamente in ciò che non sapevamo.
#
# DISTRUTTIVO. Crea utenti, installa pacchetti, tocca PostgreSQL e systemd.
# Va eseguito SOLO su macchine usa-e-getta: runner di CI, container, VM di
# prova. Mai su una macchina di lavoro.
#
# Modalità (MODE):
#   full   — l'installazione DEVE riuscire; si verifica il servizio attivo,
#            Odoo che risponde, il DB, e poi il rollback. Richiede systemd
#            funzionante (runner nativi).
#   probe  — l'installazione PUÒ fallire (container senza systemd come PID 1:
#            `setup-postgres` non riesce ad avviare il servizio). Si verifica
#            ciò che è arrivato a compimento — nomi dei pacchetti apt per
#            quell'OS, pin wkhtmltopdf per quel codename — e soprattutto che il
#            sistema resti pulito. È la sonda di portabilità (A5.1/A5.2).
#
# Variabili: MODE, BIN, ENV_FILE. I valori attesi degli artefatti seguono
# configs/ci.env.
# =============================================================================

set -euo pipefail

MODE="${MODE:-full}"
BIN="${BIN:-./target/release/odoo-installer}"
ENV_FILE="${ENV_FILE:-configs/ci.env}"

# Il test si adatta alla configurazione che gli viene data invece di assumerla:
# con `WITH_NGINX=true` verifica anche i cinque step nginx, che altrimenti
# escono al primo `if` e restano coperti solo dai mock (B-V3-7).
#
# Si legge il file con `sed`, non con `source`: è lo stesso motivo per cui
# l'installer lo fa in modo dichiarativo — un `.env` non è codice da eseguire.
env_value() {
  sed -n "s/^$1=[\"']\\?\\([^\"']*\\)[\"']\\?[[:space:]]*$/\\1/p" "$ENV_FILE" | tail -n 1
}
WITH_NGINX="$(env_value WITH_NGINX)"
WITH_NGINX="${WITH_NGINX:-false}"

# Deve combaciare con configs/ci.env. Il DB_NAME non di default è deliberato:
# vedi il commento nel file di config.
DB_NAME="${DB_NAME:-citest}"
DB_ROLE="${DB_ROLE:-odoo}"
OS_USER="${OS_USER:-odoo}"
PORT="${PORT:-8069}"
VER_SHORT="${VER_SHORT:-18}"

ODOO_HOME=/opt/odoo
INSTALL_DIR="$ODOO_HOME/odoo${VER_SHORT}"
UNIT="odoo${VER_SHORT}"
UNIT_FILE="/etc/systemd/system/${UNIT}.service"
STATE="/var/lib/odoo-installer/state.json"
# shellcheck source=scripts/ci/journal.sh
. "$(dirname "$0")/journal.sh"

WORK="$(mktemp -d)"

# Le asserzioni NON si fermano alla prima: un solo giro di CI (che dura decine
# di minuti) deve dire *tutto* ciò che non va, non solo il primo sintomo.
FAILURES=0
# Non basta CONTARLE. Le asserzioni vivono dentro `::group::`, che GitHub
# mostra collassati: chi legge il riepilogo vede «4 verifiche non superate» e
# deve andare a cercare QUALI, aprendo i gruppi uno per uno. È la lezione di
# A-R9-1 applicata a questo script — `exit != 0` non dice perché, e nemmeno un
# numero lo dice. I messaggi si accumulano e si ristampano alla fine, FUORI dai
# gruppi.
FAILED_CHECKS=()

# --- utilità -----------------------------------------------------------------

group()  { echo "::group::$*"; }
endgroup() { echo "::endgroup::"; }
info()   { echo "  ·  $*"; }
ok()     { echo "  ✔  $*"; }
fail()   { echo "  ✖  $*"; FAILURES=$((FAILURES + 1)); FAILED_CHECKS+=("$*"); }

# Legge il file di stato (0600 root) via sudo.
state_json() { sudo cat "$STATE" 2>/dev/null || echo '{}'; }

# `assert <descrizione> <comando...>` — vero se il comando esce 0.
assert() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then ok "$desc"; else fail "$desc"; fi
}

# `refute <descrizione> <comando...>` — vero se il comando esce ≠ 0.
refute() {
  local desc="$1"; shift
  if "$@" >/dev/null 2>&1; then fail "$desc"; else ok "$desc"; fi
}

pg_query() { sudo -u postgres psql -tAc "$1" 2>/dev/null || true; }

pg_reachable() { sudo -u postgres psql -tAc 'select 1' >/dev/null 2>&1; }

# La famiglia del gestore di pacchetti, letta dal sistema.
#
# Lo script gira su entrambe: rende per-famiglia le tre domande che dipendono dal
# gestore — «è installato?», «cosa c'è installato?», «dove sta il default site di
# nginx?» — e lascia tutto il resto invariato, perché tutto il resto non dipende
# dal gestore.
case "$(. /etc/os-release && echo "$ID")" in
  fedora|rhel|centos|almalinux|rocky) PKG_FAMILY=rpm ;;
  *)                                  PKG_FAMILY=deb ;;
esac
info "famiglia di pacchetti: $PKG_FAMILY"

# "Installato" con la stessa definizione che usa l'installer
# (`PackageManager::is_installed`). Non è pedanteria: `dpkg -s` esce **0** anche
# su un pacchetto rimosso che ha ancora i file di configurazione
# (`deinstall ok config-files`), e con quella definizione un purge mancato
# potrebbe passare per riuscito. Le asserzioni devono misurare ciò che
# l'installer considera presente, non qualcosa di simile.
#
# Su rpm il problema non si pone — non esiste lo stato «rimosso ma configurato»
# — e `rpm -q` è già la domanda esatta.
pkg_installed() {
  if [ "$PKG_FAMILY" = rpm ]; then
    rpm -q -- "$1" >/dev/null 2>&1
  else
    dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q 'install ok installed'
  fi
}

# --- fase 1: installazione ---------------------------------------------------

group "Installazione reale ($MODE)"
echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME")"
echo "Config: $ENV_FILE"

# Fotografia PRIMA di mutare: /opt/odoo esiste già su questo runner?
# Serve alla verifica finale (A-V3-2). Se era preesistente, il rollback deve
# lasciarla — `prepare-opt-root` la marca Preexisting — e pretendere che sparisca
# sarebbe pretendere una violazione del principio chirurgico. Sui runner e nei
# container l'attesa è che NON esista, e in quel caso dopo il rollback non deve
# esistere nemmeno alla fine.
if [ -d "$ODOO_HOME" ]; then
  OPT_ODOO_PREEXISTING=1
  info "$ODOO_HOME esisteva già prima dell'installazione"
else
  OPT_ODOO_PREEXISTING=0
  info "$ODOO_HOME assente prima dell'installazione (macchina vergine)"
fi

# L'utente di sistema c'era già? Se sì, il rollback deve LASCIARLO: è la
# protezione più importante del progetto applicata agli utenti, e va verificata
# nel verso giusto — pretendere che sparisca sarebbe pretendere una violazione.
if id "$OS_USER" >/dev/null 2>&1; then
  OS_USER_PREEXISTING=1
  info "l'utente '$OS_USER' esisteva già prima dell'installazione"
else
  OS_USER_PREEXISTING=0
  info "l'utente '$OS_USER' assente prima dell'installazione"
fi

# Nginx: cosa c'era al posto del default site, prima di noi (A-V3-5).
#
# Su rpm il default site **non è un file separato**: è un blocco `server` dentro
# `nginx.conf`, e l'installer non lo tocca (vedi `Fedora::nginx_layout`). Lì la
# domanda non si pone, e fingere che si ponga produrrebbe asserzioni su un file
# che non esiste — verdi per la ragione sbagliata.
if [ "$PKG_FAMILY" = rpm ]; then
  DEFAULT_SITE=/dev/null
else
  DEFAULT_SITE=/etc/nginx/sites-enabled/default
fi
if [ -L "$DEFAULT_SITE" ]; then
  DEFAULT_SITE_BEFORE="symlink:$(readlink "$DEFAULT_SITE")"
elif [ -f "$DEFAULT_SITE" ]; then
  DEFAULT_SITE_BEFORE="file"
else
  DEFAULT_SITE_BEFORE="assente"
fi
[ "$WITH_NGINX" = "true" ] && info "default site nginx prima dell'installazione: $DEFAULT_SITE_BEFORE"

# Fotografia dei pacchetti installati PRIMA di mutare.
#
# Serve alla verifica finale più importante e senza contabilità: **nessun
# pacchetto che c'era prima deve mancare dopo**. Non dipende da cosa abbiamo
# registrato noi, quindi non può passare per il motivo sbagliato.
pkgs_installed_now() {
  if [ "$PKG_FAMILY" = rpm ]; then
    rpm -qa --qf '%{NAME}\n' 2>/dev/null | sort -u
  else
    dpkg-query -W -f='${Package}\t${Status}\n' 2>/dev/null \
      | awk -F'\t' '$2=="install ok installed"{print $1}' | sort -u
  fi
}
pkgs_installed_now > "$WORK/pkgs-before.txt"
info "pacchetti installati prima:     $(wc -l < "$WORK/pkgs-before.txt")"

# ufw è ATTIVO? Solo allora `nginx-firewall` fa qualcosa: sui runner di default
# ufw è installato ma inattivo, e lo step esce subito (A-V3-7 mai esercitato).
if [ "$PKG_FAMILY" = rpm ]; then
  # firewalld: le verifiche sul firewall su questa famiglia non sono ancora
  # scritte (il job Fedora nasce senza firewalld installato). Dichiararlo qui
  # invece di far passare le asserzioni ufw a vuoto.
  UFW_ACTIVE=0
  [ "$WITH_NGINX" = "true" ] && info "famiglia rpm: le verifiche sul firewall non sono ancora scritte, saltate"
elif command -v ufw >/dev/null 2>&1 && sudo ufw status 2>/dev/null | grep -q "Status: active"; then
  UFW_ACTIVE=1
  sudo ufw status | sed 's/^/  ufw· /'
else
  UFW_ACTIVE=0
  [ "$WITH_NGINX" = "true" ] && info "ufw non attivo: le verifiche sul firewall verranno saltate"
fi
# L'output si cattura: da qui si legge il **diario** dell'esecuzione (quali step
# sono stati raggiunti, quali pacchetti aggiunti). Il manifesto NON serve a
# questo — dice cosa resta sul sistema, e dopo un rollback non resta nulla.
set +e
sudo "$BIN" --config "$ENV_FILE" 2>&1 | tee "$WORK/install.out"
INSTALL_RC=${PIPESTATUS[0]}
set -e
echo "exit code installazione: $INSTALL_RC"
endgroup

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -ne 0 ]; then
  fail "l'installazione doveva riuscire (exit $INSTALL_RC)"
  # Senza installazione non c'è nulla da verificare, ma il rollback va provato
  # lo stesso: deve ripulire ciò che la run fallita ha lasciato.
fi

# --- fase 2: il sistema installato funziona (solo MODE=full) -----------------

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
  group "Verifica dell'installazione"

  assert "utente di sistema '$OS_USER' creato" id "$OS_USER"
  assert "sorgenti in $INSTALL_DIR" test -d "$INSTALL_DIR/odoo"
  assert "virtualenv creato" test -x "$INSTALL_DIR/sandbox/bin/python3"
  assert "config generata" test -f "$INSTALL_DIR/odoo${VER_SHORT}.conf"
  assert "unit systemd installata" test -f "$UNIT_FILE"
  assert "servizio attivo" systemctl is-active --quiet "$UNIT"
  assert "servizio abilitato" systemctl is-enabled --quiet "$UNIT"
  assert "wkhtmltopdf installato" pkg_installed wkhtmltox

  # Il database esiste e ha lo schema Odoo inizializzato.
  if [ "$(pg_query "select 1 from pg_database where datname='$DB_NAME'")" = "1" ]; then
    ok "database '$DB_NAME' creato"
  else
    fail "database '$DB_NAME' assente"
  fi
  if [ "$(pg_query "select 1 from pg_roles where rolname='$DB_ROLE'")" = "1" ]; then
    ok "ruolo PostgreSQL '$DB_ROLE' creato"
  else
    fail "ruolo PostgreSQL '$DB_ROLE' assente"
  fi
  if [ "$(sudo -u postgres psql -tAd "$DB_NAME" -c \
        "select 1 from information_schema.tables where table_name='ir_module_module'" 2>/dev/null)" = "1" ]; then
    ok "schema Odoo inizializzato (ir_module_module presente)"
  else
    fail "schema Odoo non inizializzato"
  fi

  # Odoo risponde davvero sulla porta. Il servizio "attivo" non basta: il
  # processo può essere su e morire subito dopo per una config sbagliata.
  # `/` risponde con un redirect (303/302) verso /odoo o /web: va benissimo
  # come prova di "vivo".
  http=""
  for _ in $(seq 1 30); do
    http="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:${PORT}/" 2>/dev/null || echo 000)"
    case "$http" in 200|301|302|303|307) break ;; esac
    sleep 2
  done
  case "$http" in
    200|301|302|303|307) ok "Odoo risponde su :$PORT (HTTP $http)" ;;
    *) fail "Odoo non risponde su :$PORT (ultimo codice: ${http:-nessuno})"
       sudo journalctl -u "$UNIT" -n 60 --no-pager || true ;;
  esac

  # A-R5-1: lo stato sopravvive al successo ed è marcato concluso. È ciò che
  # rende possibile disinstallare più tardi; se questo assert cade, il comando
  # `rollback` qui sotto non avrebbe nulla da consumare.
  assert "il manifesto di disinstallazione è rimasto sul disco" sudo test -f "$STATE"
  if [ "$(state_json | jq -r '.finished')" = "true" ]; then
    ok "il manifesto è marcato 'finished'"
  else
    fail "il manifesto NON è marcato 'finished': il rollback lo leggerebbe come \
installazione interrotta"
  fi
  if [ "$(state_json | jq -r '.config.db_name')" = "$DB_NAME" ]; then
    ok "il manifesto porta la configurazione reale (db_name=$DB_NAME)"
  else
    fail "il manifesto non porta il db_name reale: il rollback non saprebbe cosa \
rimuovere (regressione A-R4-1)"
  fi

  endgroup
fi

# --- fase 2-ter: la fase Nginx, quando è richiesta (B-V3-7) ------------------
#
# Fino a questo punto la CI reale non l'aveva **mai** eseguita: `configs/ci.env`
# ha `WITH_NGINX="false"`, quindi i cinque step nginx uscivano al primo `if` in
# ogni installazione mai fatta su una macchina vera. Il vhost non veniva mai
# scritto né validato da `nginx -t`, il default site mai rimosso né ripristinato.
# R11 (A-V3-5) e R12 (A-V3-6) vivevano interamente su mock — ed è esattamente la
# situazione in cui, in questo progetto, i difetti sono sopravvissuti: A1.4
# (ordine del reload nginx) è stato trovato da un e2e, non da una rilettura.

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ] && [ "$WITH_NGINX" = "true" ]; then
  group "Verifica Nginx"

  VHOST="/etc/nginx/sites-available/odoo${VER_SHORT}"
  VHOST_LINK="/etc/nginx/sites-enabled/odoo${VER_SHORT}"

  assert "vhost generato in $VHOST" sudo test -f "$VHOST"
  assert "sito abilitato ($VHOST_LINK)" sudo test -L "$VHOST_LINK"
  assert "la configurazione nginx è valida (nginx -t)" sudo nginx -t
  assert "nginx attivo" systemctl is-active --quiet nginx

  # A-V3-12: i log del vhost portano la versione, non un `odoo18` cablato.
  if sudo grep -q "odoo${VER_SHORT}.access.log" "$VHOST"; then
    ok "i log del vhost seguono la versione installata (A-V3-12)"
  else
    fail "i log del vhost non portano la versione: due istanze si scriverebbero addosso"
    sudo grep -n "access_log\|error_log" "$VHOST" || true
  fi

  # A-V3-6: il vhost NON promette TLS. Un blocco 443 verso certificati
  # inesistenti farebbe fallire `nginx -t` — e infatti sopra passa.
  if sudo grep -qE "^\s*listen\s+443|^\s*ssl_certificate" "$VHOST"; then
    fail "il vhost contiene direttive TLS: non le genera l'installer (A-V3-6)"
  else
    ok "il vhost non promette TLS (è compito di certbot --nginx)"
  fi

  # Il default site è stato tolto di mezzo: è ciò che libera la porta 80.
  refute "il default site è stato disattivato" sudo test -e "$DEFAULT_SITE"

  # Firewall: la porta 80 dev'essere stata aperta (A-V3-7).
  #
  # È la verifica che il confronto per token funziona su una macchina vera. Con
  # il vecchio `status.contains("80/tcp")`, la presenza di `8080/tcp` faceva
  # risultare la 80 già aperta: non entrava nel delta, non veniva aperta, e
  # nginx restava irraggiungibile **senza alcun errore**.
  if [ "$UFW_ACTIVE" = "1" ]; then
    if sudo ufw status | awk '{print $1}' | grep -qx "80/tcp"; then
      ok "regola ufw 80/tcp aperta (A-V3-7: non confusa con 8080/tcp)"
    else
      fail "la porta 80 NON è stata aperta su ufw: nginx resta irraggiungibile"
      sudo ufw status || true
    fi
  fi

  # E la 80 serve Odoo attraverso il proxy: è l'unica prova che l'intera catena
  # (vhost + reload + upstream) funziona davvero.
  http80=""
  for _ in $(seq 1 15); do
    http80="$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1/" 2>/dev/null || echo 000)"
    case "$http80" in 200|301|302|303|307) break ;; esac
    sleep 2
  done
  case "$http80" in
    200|301|302|303|307) ok "Odoo risponde attraverso Nginx sulla porta 80 (HTTP $http80)" ;;
    *) fail "la porta 80 non serve Odoo (ultimo codice: ${http80:-nessuno})"
       sudo nginx -T 2>/dev/null | head -n 40 || true ;;
  esac

  endgroup
fi

# --- fase 2-bis: una seconda installazione NON deve toccare il manifesto -----
#
# A-V3-1. Prima, rilanciare l'installer su un'istanza già installata riscriveva
# il manifesto con ogni artefatto marcato `Preexisting` — perché è ciò che gli
# snapshot vedono, correttamente — e da lì `rollback` faceva 24 undo NO-OP,
# dichiarava «nessun residuo», cancellava lo stato e lasciava Odoo installato
# per sempre. Lo scenario finiva con il test **verde**: è il motivo per cui
# serve provarlo qui e non solo su mock.
#
# Si verificano tre cose, in ordine di importanza: che la seconda esecuzione
# fallisca; che il manifesto sia rimasto **identico**; e che il messaggio
# indirizzi l'utente da qualche parte invece di lasciarlo a indovinare.

if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
  group "Seconda installazione: deve essere rifiutata (A-V3-1)"

  sudo cp "$STATE" "$WORK/manifest-before.json"

  set +e
  sudo "$BIN" --config "$ENV_FILE" > "$WORK/second-install.log" 2>&1
  SECOND_RC=$?
  set -e
  echo "exit code seconda installazione: $SECOND_RC"

  if [ "$SECOND_RC" -ne 0 ]; then
    ok "la seconda installazione è stata rifiutata (exit $SECOND_RC)"
  else
    fail "la seconda installazione è stata ACCETTATA: il manifesto è a rischio (A-V3-1)"
  fi

  sudo cp "$STATE" "$WORK/manifest-after.json"
  if sudo cmp -s "$WORK/manifest-before.json" "$WORK/manifest-after.json"; then
    ok "il manifesto è rimasto identico byte-per-byte"
  else
    fail "il manifesto è cambiato dopo un'installazione rifiutata: \
l'istanza potrebbe non essere più disinstallabile (A-V3-1)"
    sudo diff "$WORK/manifest-before.json" "$WORK/manifest-after.json" || true
  fi

  # Il rifiuto deve venire dal MANIFESTO, non da un effetto collaterale.
  #
  # A-R9-1: la prima versione di questo blocco si accontentava di `exit != 0` e
  # di un messaggio che citasse `rollback`. Passava — anzi, falliva — per la
  # ragione sbagliata: l'installazione veniva respinta da `check_ports` («porta
  # 8069 già in uso»), perché Odoo era in ascolto, e il controllo sul manifesto
  # non veniva raggiunto mai. Un'uscita non-zero non dice *perché*, e qui il
  # perché è tutto: «libera la porta» manda l'utente a fermare Odoo, non a
  # disinstallarlo.
  if grep -q "installazione completata su questa macchina" "$WORK/second-install.log"; then
    ok "il rifiuto viene dal manifesto (A-V3-1), non da un effetto collaterale"
  else
    fail "la seconda installazione è stata respinta, ma NON dal controllo sul manifesto: \
il rifiuto di A-V3-1 non è stato raggiunto (A-R9-1)"
    tail -n 20 "$WORK/second-install.log" || true
  fi

  if grep -q -e 'porta .* in uso' "$WORK/second-install.log"; then
    fail "il rifiuto arriva dal controllo sulla porta: è una CONSEGUENZA \
dell'installazione esistente, non la causa — l'utente viene mandato a fermare Odoo (A-R9-1)"
  else
    ok "nessuna diagnosi fuorviante sulla porta"
  fi

  if grep -q -e 'rollback' -e '--force' "$WORK/second-install.log"; then
    ok "il rifiuto indica come procedere (rollback o --force)"
  else
    fail "il rifiuto non dice all'utente cosa fare"
    tail -n 20 "$WORK/second-install.log" || true
  fi

  endgroup
fi

# --- fase 3: il diario dell'esecuzione, letto dal LOG ------------------------
#
# Il delta è l'insieme dei pacchetti che NON c'erano prima di noi: il rollback
# deve purgare quelli e SOLO quelli. Leggerlo dall'output invece di scriverlo a
# mano rende l'asserzione indipendente dall'immagine: su un runner GitHub
# `git`/`curl`/`build-essential` sono già installati, su un container Debian
# minimale no.
#
# **Dal log e non dal manifesto**, e la distinzione è il punto. Il manifesto dice
# *cosa c'è ancora sul sistema*: quando un rollback annulla uno step, quel record
# sparisce — è ciò che impedisce a un rilancio di saltare artefatti che non
# esistono più (A-R8-1). Il diario di *cosa è stato fatto* vive invece nel log,
# che non viene riscritto. In `MODE=probe` l'installazione fallisce e si annulla
# da sé, quindi il manifesto è (giustamente) vuoto quando arriviamo qui: leggerlo
# darebbe zero pacchetti e tutte le verifiche di pulizia passerebbero a vuoto.

group "Diario dell'esecuzione (dal log)"

# Il parsing vive in journal.sh, esercitato da selftest-journal.sh nella CI
# veloce: un pattern che non combacia non dà errore, dà zero risultati — e zero
# pacchetti da verificare si presenta come una verifica superata.
sed_out="$WORK/install.txt"
journal_strip_ansi "$WORK/install.out" > "$sed_out"

readonly DEP_STEP='install-system-dependencies'
journal_steps "$sed_out" > "$WORK/steps.txt"
journal_packages "$sed_out" 'pacchetti aggiunti da noi' "$DEP_STEP" > "$WORK/delta.txt"
journal_packages "$sed_out" 'pacchetti già presenti, mai toccati' "$DEP_STEP" \
  > "$WORK/preexisting.txt"

info "step completati:                $(wc -l < "$WORK/steps.txt")"
info "delta (installati da noi):      $(wc -l < "$WORK/delta.txt") pacchetti"
info "preesistenti (mai toccati):     $(wc -l < "$WORK/preexisting.txt") pacchetti"

if [ ! -s "$WORK/delta.txt" ] && [ "$MODE" = "full" ]; then
  fail "nessun pacchetto nel delta: il diario non è stato letto correttamente, \
e le verifiche di pulizia passerebbero a vuoto"
fi

# In MODE=probe l'installazione si ferma presto per costruzione (niente systemd
# come PID 1 → `setup-postgres` non avvia il servizio). Senza questo controllo,
# un fallimento allo step 1 supererebbe tutte le verifiche di pulizia per il
# motivo sbagliato: non c'è nulla da pulire perché non è stato fatto nulla.
# Questi due step sono ciò che la sonda esiste per esercitare: i nomi dei
# pacchetti apt di quell'OS (A5.1) e il pin wkhtmltopdf del suo codename
# (A5.2, A-RT-1).
if [ "$MODE" = "probe" ]; then
  for step in install-system-dependencies install-wkhtmltopdf; do
    if grep -qx "$step" "$WORK/steps.txt"; then
      ok "la sonda ha superato '$step' su questo OS"
    else
      fail "la sonda non ha raggiunto '$step': la portabilità non è stata verificata"
    fi
  done
fi
endgroup

# --- fase 4: rollback --------------------------------------------------------

group "Rollback"
set +e
sudo "$BIN" rollback --yes 2>&1 | tee "$WORK/rollback.out"
ROLLBACK_RC=${PIPESTATUS[0]}
set -e
echo "exit code rollback: $ROLLBACK_RC"
endgroup

if [ "$ROLLBACK_RC" -eq 0 ]; then
  ok "il rollback è terminato senza errori"
else
  fail "il rollback è uscito con $ROLLBACK_RC"
fi
# Due esiti sono entrambi corretti, e distinguerli conta.
#
# «Nessun residuo» = c'era un'installazione registrata e l'ha annullata tutta.
# «Nessuna installazione da annullare» = non c'era nulla da fare — ed è il caso
# NORMALE in `MODE=probe`, dove l'installazione fallisce e **si annulla da sé**
# durante l'esecuzione: quando arriviamo qui il sistema è già pulito e il
# manifesto è già sparito. Pretendere «Nessun residuo» significherebbe pretendere
# che ci fosse ancora qualcosa da annullare.
if grep -q "Nessun residuo" "$WORK/rollback.out"; then
  ok "il rollback dichiara nessun residuo"
elif grep -q "Nessuna installazione da annullare" "$WORK/rollback.out"; then
  if [ "$MODE" = "full" ] && [ "$INSTALL_RC" -eq 0 ]; then
    fail "non c'era nulla da annullare, ma l'installazione era riuscita: il manifesto \
di disinstallazione è sparito quando invece doveva restare"
  else
    ok "niente da annullare: il rollback in-process aveva già ripulito tutto"
  fi
else
  fail "il rollback ha lasciato residui (vedi il report qui sopra)"
fi

# --- fase 5: il sistema è tornato pulito -------------------------------------
#
# Sono gli stessi controlli fatti a mano su Multipass, ora automatici. Il punto
# non è "il comando è uscito 0": è che sul sistema non resti niente di nostro.

group "Verifica di pulizia post-rollback"

if [ "$OS_USER_PREEXISTING" = "1" ]; then
  # Preesistente: NON è nostro da cancellare. È l'anti-drop applicato agli
  # utenti, e qui l'asserzione va nel verso opposto.
  assert "l'utente preesistente '$OS_USER' è sopravvissuto al rollback" id "$OS_USER"
else
  refute "l'utente di sistema '$OS_USER' è stato rimosso" id "$OS_USER"
  refute "il gruppo '$OS_USER' è stato rimosso" getent group "$OS_USER"
fi
refute "la directory di installazione è sparita" test -d "$INSTALL_DIR"
refute "l'unit systemd è stata rimossa" test -f "$UNIT_FILE"
refute "il servizio non è più attivo" systemctl is-active --quiet "$UNIT"
refute "il manifesto è stato consumato" sudo test -f "$STATE"
refute "wkhtmltopdf è stato purgato" pkg_installed wkhtmltox

if pg_reachable; then
  if [ -z "$(pg_query "select 1 from pg_database where datname='$DB_NAME'")" ]; then
    ok "database '$DB_NAME' droppato"
  else
    fail "database '$DB_NAME' ancora presente"
  fi
  if [ -z "$(pg_query "select 1 from pg_roles where rolname='$DB_ROLE'")" ]; then
    ok "ruolo PostgreSQL '$DB_ROLE' rimosso"
  else
    fail "ruolo PostgreSQL '$DB_ROLE' ancora presente"
  fi
else
  info "PostgreSQL non raggiungibile: verifiche su DB/ruolo saltate"
  info "(atteso in MODE=probe, dove il servizio non è mai partito)"
fi

# PostgreSQL resta INSTALLATO: senza --aggressive-rollback il rollback fa solo
# stop + disable, perché quelli sono reversibili e un purge no (D3). Che resti
# è il comportamento corretto, non un residuo.
if pkg_installed postgresql; then
  ok "PostgreSQL resta installato (corretto: purge solo con --aggressive-rollback)"
fi

# Nginx: la config del cliente torna com'era (B-V3-7).
#
# È la metà che conta di A-V3-5. Il default site è **config preesistente di
# terzi**: rimuoverlo per liberare la porta 80 è lecito, non rimetterlo a posto
# no. Fino a questa fase nessuna esecuzione reale l'aveva mai verificato.
if [ "$WITH_NGINX" = "true" ]; then
  refute "il vhost è stato rimosso" sudo test -f "/etc/nginx/sites-available/odoo${VER_SHORT}"
  refute "il sito è stato disabilitato" sudo test -e "/etc/nginx/sites-enabled/odoo${VER_SHORT}"

  case "$DEFAULT_SITE_BEFORE" in
    assente)
      refute "nessun default site inventato dal rollback" sudo test -e "$DEFAULT_SITE"
      ;;
    symlink:*)
      atteso="${DEFAULT_SITE_BEFORE#symlink:}"
      if [ -L "$DEFAULT_SITE" ] && [ "$(readlink "$DEFAULT_SITE")" = "$atteso" ]; then
        ok "il default site è tornato al suo target originale ($atteso)"
      else
        fail "il default site NON è stato ripristinato com'era: atteso symlink → $atteso, \
trovato $( [ -e "$DEFAULT_SITE" ] && ls -ld "$DEFAULT_SITE" || echo assente )"
      fi
      ;;
    file)
      if [ -f "$DEFAULT_SITE" ] && [ ! -L "$DEFAULT_SITE" ]; then
        ok "il default site (file regolare) è stato rimesso al suo posto"
      else
        fail "un default site che era un FILE non è tornato tale: è la perdita di \
configurazione che A-V3-5 descrive"
      fi
      ;;
  esac

  # Firewall dopo il rollback: si chiude ciò che abbiamo aperto, e **solo**
  # quello. Una regola preesistente del cliente non si tocca mai — è la stessa
  # regola del delta apt, applicata al firewall.
  if [ "$UFW_ACTIVE" = "1" ]; then
    if sudo ufw status | awk '{print $1}' | grep -qx "80/tcp"; then
      fail "la regola 80/tcp aperta da noi è rimasta dopo il rollback"
    else
      ok "la regola ufw 80/tcp è stata richiusa"
    fi
    if sudo ufw status | awk '{print $1}' | grep -qx "8080/tcp"; then
      ok "la regola preesistente 8080/tcp è intatta"
    else
      fail "il rollback ha rimosso una regola ufw che non aveva aperto lui"
    fi
  fi

  # nginx sopravvive e resta servibile: il rollback non deve lasciare il
  # servizio del cliente con una config rotta (A1.4).
  if pkg_installed nginx || pkg_installed nginx-core || pkg_installed nginx-full; then
    ok "nginx resta installato (purge solo con --aggressive-rollback)"
  fi
  assert "la configurazione nginx è valida anche dopo il rollback" sudo nginx -t
fi

# Il delta apt: purgato per intero.
delta_left=0
while read -r pkg; do
  if [ -z "$pkg" ]; then continue; fi
  if pkg_installed "$pkg"; then
    fail "pacchetto del delta ancora installato: $pkg"
    delta_left=$((delta_left + 1))
  fi
done < "$WORK/delta.txt"
# `if`, non `[ ] && ok`: sotto `set -e` un test falso come ultimo comando di una
# lista `&&` farebbe uscire lo script prima del report finale.
if [ "$delta_left" -eq 0 ]; then
  ok "tutti i pacchetti del delta sono stati purgati"
fi

# I preesistenti: mai toccati. È la promessa chirurgica, dal lato opposto —
# un rollback che purga troppo è grave quanto uno che purga troppo poco.
preexisting_lost=0
while read -r pkg; do
  if [ -z "$pkg" ]; then continue; fi
  if ! pkg_installed "$pkg"; then
    fail "pacchetto PREESISTENTE rimosso dal rollback: $pkg"
    preexisting_lost=$((preexisting_lost + 1))
  fi
done < "$WORK/preexisting.txt"
if [ "$preexisting_lost" -eq 0 ]; then
  ok "nessun pacchetto preesistente è stato toccato"
fi

# La stessa promessa, verificata **senza contabilità**: si confronta l'insieme
# dei pacchetti installati prima con quello di adesso. Non dipende da cosa
# abbiamo registrato noi, quindi non può passare per il motivo sbagliato — è la
# differenza fra «i pacchetti che dicevamo di aver aggiunto sono spariti» e
# «niente di ciò che c'era è sparito».
pkgs_installed_now > "$WORK/pkgs-after.txt"
if perduti="$(comm -23 "$WORK/pkgs-before.txt" "$WORK/pkgs-after.txt")" && [ -z "$perduti" ]; then
  ok "nessun pacchetto presente prima dell'installazione è stato rimosso"
else
  fail "il rollback ha rimosso pacchetti che c'erano già: $(echo "$perduti" | tr '\n' ' ')"
fi

# /opt/odoo: su macchina vergine, dopo il rollback NON deve esistere.
#
# Fino ad A-V3-2 questa verifica accettava una whitelist (`.installer.log`,
# `.installer.lock`) e dichiarava OK una directory sopravvissuta: metteva per
# iscritto il residuo come comportamento atteso, ed è il motivo per cui la CI
# non ha mai trovato il difetto. I due file erano lì perché lock e log vivevano
# dentro la home; ora vivono in /run e /var/log, la directory non ha più alcuna
# ragione di sopravvivere, e l'undo di `prepare-opt-root` può finalmente
# attivarsi.
#
# È questa riga ad aver trovato A-R5-3: `.cache` (la cache di pip, che nasceva
# nella home dell'utente odoo) e `.local` (il filestore, che Odoo si creava da
# sé, fuori da ogni step e quindi non annullabile). Chiuse in R6 dai due lati
# opposti — la cache non nasce più qui (`pip --cache-dir` dentro il venv), il
# filestore è ora lo step `setup-data-dir` con il suo PreState. La guardia di
# regressione resta, ora più stretta: qualunque contenuto è un residuo, e lo è
# anche la directory stessa.
#
# Nota: vale perché il runner parte da una macchina senza /opt/odoo. Se la
# directory fosse preesistente, `prepare-opt-root` la marcherebbe Preexisting e
# NON rimuoverla sarebbe il comportamento corretto — per questo si verifica
# che fosse assente prima di installare.
if [ "${OPT_ODOO_PREEXISTING:-0}" = "1" ]; then
  ok "$ODOO_HOME era preesistente: il rollback non deve rimuoverla (nessuna asserzione)"
elif [ -d "$ODOO_HOME" ]; then
  leftovers="$(sudo ls -A "$ODOO_HOME" | tr '\n' ' ' || true)"
  fail "$ODOO_HOME è sopravvissuta al rollback (contenuto: ${leftovers:-vuota}) — A-V3-2"
else
  ok "$ODOO_HOME non esiste più: il perimetro è tornato com'era"
fi

endgroup

# --- esito -------------------------------------------------------------------

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "=== INTEGRAZIONE OK ($MODE): installazione e rollback verificati sul sistema reale ==="
  exit 0
fi
echo "=== INTEGRAZIONE FALLITA ($MODE): $FAILURES verifiche non superate ==="
echo "Verifiche fallite:"
for check in "${FAILED_CHECKS[@]}"; do
  echo "  ✖  $check"
  # `::error::` diventa un'annotazione in cima all'esecuzione: visibile senza
  # aprire nulla, che è il punto.
  echo "::error::$check"
done
echo "Log dell'installer:"
sudo tail -n 100 /var/log/odoo-installer.log 2>/dev/null || echo "(nessun log)"
exit 1
