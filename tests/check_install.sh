#!/usr/bin/env bash
# =============================================================================
# tests/check_install.sh — Suite di verifica installazione Odoo 18
# =============================================================================
# Uso:
#   sudo bash tests/check_install.sh [--config /path/to/odoo18.conf] [--verbose]
#
# Exit codes:
#   0  Tutti i test superati
#   1  Uno o più test falliti
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Valori di default (override via argomenti o variabili d'ambiente)
# ---------------------------------------------------------------------------
ODOO_VERSION="${ODOO_VERSION:-18}"
ODOO_USER="${ODOO_USER:-odoo}"
ODOO_HOME="${ODOO_HOME:-/opt/odoo}"
ODOO_PORT="${ODOO_PORT:-8069}"
ODOO_CONF="${ODOO_CONF:-/opt/odoo/odoo18/odoo18.conf}"
DB_USER="${DB_USER:-odoo}"
VERBOSE="${VERBOSE:-false}"
DRY_RUN="${DRY_RUN:-false}"

# ---------------------------------------------------------------------------
# Colori e formattazione
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

# ---------------------------------------------------------------------------
# Contatori globali
# ---------------------------------------------------------------------------
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0
FAILED_TESTS=()

# ---------------------------------------------------------------------------
# Funzioni di logging
# ---------------------------------------------------------------------------
log()     { echo -e "${DIM}[INFO]${RESET}  $*"; }
verbose() { [[ "$VERBOSE" == "true" ]] && echo -e "${DIM}        $*${RESET}" || true; }
warn()    { echo -e "${YELLOW}[WARN]${RESET}  $*"; }
error()   { echo -e "${RED}[ERROR]${RESET} $*" >&2; }

# Esegue un comando oppure lo stampa in dry-run
run() {
  if [[ "$DRY_RUN" == "true" ]]; then
    echo -e "${DIM}[DRY-RUN]${RESET} $*"
  else
    "$@"
  fi
}

# ---------------------------------------------------------------------------
# Funzioni di test
# ---------------------------------------------------------------------------

# Registra un test come superato
pass() {
  local name="$1"
  TESTS_RUN=$(( TESTS_RUN + 1 ))
  TESTS_PASSED=$(( TESTS_PASSED + 1 ))
  echo -e "  ${GREEN}✔${RESET}  $name"
}

# Registra un test come fallito
fail() {
  local name="$1"
  local reason="${2:-}"
  TESTS_RUN=$(( TESTS_RUN + 1 ))
  TESTS_FAILED=$(( TESTS_FAILED + 1 ))
  FAILED_TESTS+=("$name")
  echo -e "  ${RED}✘${RESET}  $name"
  [[ -n "$reason" ]] && echo -e "        ${DIM}↳ $reason${RESET}"
}

# Salta un test (dipendenza non soddisfatta)
skip() {
  local name="$1"
  local reason="${2:-}"
  TESTS_SKIPPED=$(( TESTS_SKIPPED + 1 ))
  echo -e "  ${YELLOW}⊘${RESET}  ${DIM}$name (skipped${reason:+: $reason})${RESET}"
}

# Intestazione di ogni gruppo
section() {
  echo ""
  echo -e "${CYAN}${BOLD}▶ $*${RESET}"
  echo -e "${DIM}$(printf '─%.0s' $(seq 1 60))${RESET}"
}

# Esegue un test generico: check <nome> <comando>
check() {
  local name="$1"; shift
  if "$@" &>/dev/null; then
    pass "$name"
    return 0
  else
    fail "$name"
    return 1
  fi
}

# ---------------------------------------------------------------------------
# Parsing argomenti
# ---------------------------------------------------------------------------
parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --config)     ODOO_CONF="$2";  shift 2 ;;
      --odoo-home)  ODOO_HOME="$2";  shift 2 ;;
      --odoo-user)  ODOO_USER="$2";  shift 2 ;;
      --port)       ODOO_PORT="$2";  shift 2 ;;
      --verbose|-v) VERBOSE="true";  shift   ;;
      --dry-run)    DRY_RUN="true";    shift   ;;
      --help|-h)
        echo "Uso: $0 [--config FILE] [--odoo-home DIR] [--odoo-user USER] [--port PORT] [--verbose]"
        exit 0
        ;;
      *) warn "Argomento sconosciuto: $1"; shift ;;
    esac
  done
}

# ---------------------------------------------------------------------------
# GRUPPO 1 — Sistema operativo e privilegi
# ---------------------------------------------------------------------------
check_system() {
  section "Sistema operativo e privilegi"

  # Root check
  if [[ $EUID -eq 0 ]]; then
    pass "Eseguito come root"
  else
    fail "Eseguito come root" "Rilanciare con sudo"
  fi

  # OS supportato
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    local os_id="${ID:-unknown}"
    if [[ "$os_id" =~ ^(ubuntu|debian)$ ]]; then
      pass "OS supportato: $PRETTY_NAME"
    else
      fail "OS supportato" "Rilevato '$os_id' — atteso ubuntu o debian"
    fi
  else
    fail "OS supportato" "/etc/os-release non trovato"
  fi

  # Architettura
  local arch
  arch="$(uname -m)"
  if [[ "$arch" == "x86_64" ]]; then
    pass "Architettura: $arch"
  else
    warn "Architettura non standard: $arch (potrebbero esserci problemi con wkhtmltopdf)"
    pass "Architettura: $arch"
  fi

  # Spazio disco disponibile (minimo 5 GB)
  local free_kb
  free_kb=$(df -k "$ODOO_HOME" 2>/dev/null | awk 'NR==2{print $4}' || df -k / | awk 'NR==2{print $4}')
  local free_gb=$(( free_kb / 1024 / 1024 ))
  if [[ $free_gb -ge 5 ]]; then
    pass "Spazio disco libero: ${free_gb} GB (≥ 5 GB richiesti)"
  else
    fail "Spazio disco libero" "${free_gb} GB disponibili, minimo 5 GB richiesti"
  fi

  # RAM disponibile (minimo 1 GB)
  local ram_mb
  ram_mb=$(awk '/MemTotal/ {print int($2/1024)}' /proc/meminfo)
  if [[ $ram_mb -ge 1024 ]]; then
    pass "RAM totale: ${ram_mb} MB (≥ 1024 MB richiesti)"
  else
    fail "RAM totale" "${ram_mb} MB disponibili, minimo 1024 MB richiesti"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 2 — Dipendenze di sistema (apt)
# ---------------------------------------------------------------------------
check_dependencies() {
  section "Dipendenze di sistema"

  local packages=(
    git python3 python3-pip python3-venv python3-dev python3-wheel python3-setuptools
    build-essential wget libfreetype6-dev libxml2-dev libzip-dev
    libldap2-dev libsasl2-dev node-less libjpeg-dev zlib1g-dev libpq-dev
    libxslt1-dev libtiff5-dev libopenjp2-7-dev liblcms2-dev
    libwebp-dev libharfbuzz-dev libfribidi-dev libxcb1-dev
  )

  local all_ok=true
  for pkg in "${packages[@]}"; do
    if dpkg -s "$pkg" &>/dev/null; then
      verbose "Pacchetto installato: $pkg"
    else
      fail "Pacchetto apt: $pkg" "dpkg non lo rileva come installato"
      all_ok=false
    fi
  done
  [[ "$all_ok" == "true" ]] && pass "Tutti i pacchetti apt richiesti presenti"

  # Python 3 versione minima (3.10)
  local py_ver
  py_ver=$(python3 -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')" 2>/dev/null || echo "0.0")
  local py_major py_minor
  py_major=$(echo "$py_ver" | cut -d. -f1)
  py_minor=$(echo "$py_ver" | cut -d. -f2)
  if [[ $py_major -ge 3 && $py_minor -ge 10 ]]; then
    pass "Python versione: $py_ver (≥ 3.10)"
  else
    fail "Python versione" "Trovato $py_ver, richiesta ≥ 3.10"
  fi

  # Node.js (opzionale ma utile per assets)
  if command -v node &>/dev/null; then
    local node_ver
    node_ver=$(node --version)
    pass "Node.js disponibile: $node_ver"
  else
    skip "Node.js disponibile" "non installato (opzionale)"
  fi

  # wkhtmltopdf
  if command -v wkhtmltopdf &>/dev/null; then
    local wk_ver
    wk_ver=$(wkhtmltopdf --version 2>&1 | head -1)
    pass "wkhtmltopdf: $wk_ver"
  else
    fail "wkhtmltopdf" "non trovato in PATH — report PDF non disponibili"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 3 — Utente e struttura directory
# ---------------------------------------------------------------------------
check_user_and_dirs() {
  section "Utente di sistema e struttura directory"

  # Utente odoo esiste
  if id -u "$ODOO_USER" &>/dev/null; then
    pass "Utente '$ODOO_USER' esiste"
  else
    fail "Utente '$ODOO_USER' esiste" "useradd non eseguito"
    return  # le verifiche seguenti non avrebbero senso
  fi

  # Shell dell'utente deve essere /bin/false (no login interattivo)
  local user_shell
  user_shell=$(getent passwd "$ODOO_USER" | cut -d: -f7)
  if [[ "$user_shell" == "/bin/false" || "$user_shell" == "/usr/sbin/nologin" ]]; then
    pass "Shell utente '$ODOO_USER': $user_shell (sicura)"
  else
    fail "Shell utente '$ODOO_USER'" "shell '$user_shell' permette login interattivo"
  fi

  # Home directory
  local user_home
  user_home=$(getent passwd "$ODOO_USER" | cut -d: -f6)
  if [[ -d "$user_home" ]]; then
    pass "Home utente: $user_home"
  else
    fail "Home utente" "'$user_home' non esiste"
  fi

  # Directory principali
  local dirs=(
    "${ODOO_HOME}/odoo${ODOO_VERSION}"
    "${ODOO_HOME}/odoo${ODOO_VERSION}/odoo"
    "${ODOO_HOME}/odoo${ODOO_VERSION}/repos/modules"
    "${ODOO_HOME}/odoo${ODOO_VERSION}/sandbox"
  )

  for dir in "${dirs[@]}"; do
    if [[ -d "$dir" ]]; then
      pass "Directory esiste: $dir"
    else
      fail "Directory esiste" "'$dir' non trovata"
    fi
  done

  # Proprietà directory
  local owner
  owner=$(stat -c '%U' "${ODOO_HOME}/odoo${ODOO_VERSION}" 2>/dev/null || echo "unknown")
  if [[ "$owner" == "$ODOO_USER" ]]; then
    pass "Proprietà directory: $owner"
  else
    fail "Proprietà directory" "atteso '$ODOO_USER', trovato '$owner'"
  fi

  # Log directory
  if [[ -d /var/log/odoo ]]; then
    pass "Log directory: /var/log/odoo"
  else
    fail "Log directory" "/var/log/odoo non esiste"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 4 — PostgreSQL
# ---------------------------------------------------------------------------
check_postgres() {
  section "PostgreSQL"

  # Servizio attivo
  if systemctl is-active --quiet postgresql; then
    pass "Servizio postgresql attivo"
  else
    fail "Servizio postgresql attivo" "systemctl is-active postgresql fallito"
    return
  fi

  # Versione PostgreSQL (minimo 12)
  local pg_ver
  pg_ver=$(psql --version 2>/dev/null | awk '{print $3}' | cut -d. -f1 || echo "0")
  if [[ $pg_ver -ge 12 ]]; then
    pass "PostgreSQL versione: $pg_ver (≥ 12)"
  else
    fail "PostgreSQL versione" "Trovato '$pg_ver', richiesta ≥ 12"
  fi

  # Ruolo PostgreSQL per l'utente Odoo
  if sudo -u postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname='${DB_USER}'" 2>/dev/null | grep -q 1; then
    pass "Ruolo PostgreSQL '${DB_USER}' esiste"
  else
    fail "Ruolo PostgreSQL '${DB_USER}'" "createuser non eseguito"
  fi

  # Connessione locale (il ruolo può connettersi)
  if sudo -u "$ODOO_USER" psql -c '\q' postgres &>/dev/null; then
    pass "Connessione PostgreSQL come '$ODOO_USER': ok"
  else
    fail "Connessione PostgreSQL" "il ruolo '${DB_USER}' non riesce a connettersi"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 5 — Odoo: clone, virtualenv, librerie Python
# ---------------------------------------------------------------------------
check_odoo_install() {
  section "Installazione Odoo"

  local odoo_dir="${ODOO_HOME}/odoo${ODOO_VERSION}/odoo"
  local sandbox_dir="${ODOO_HOME}/odoo${ODOO_VERSION}/sandbox"

  # Repo clonato
  if [[ -f "${odoo_dir}/odoo-bin" ]]; then
    pass "odoo-bin trovato: ${odoo_dir}/odoo-bin"
  else
    fail "odoo-bin trovato" "${odoo_dir}/odoo-bin non esiste"
  fi

  # Branch corretto
  if git -C "$odoo_dir" rev-parse --abbrev-ref HEAD 2>/dev/null | grep -q "${ODOO_VERSION}.0"; then
    local branch
    branch=$(git -C "$odoo_dir" rev-parse --abbrev-ref HEAD)
    pass "Branch git: $branch"
  else
    fail "Branch git" "non è il branch ${ODOO_VERSION}.0"
  fi

  # Virtualenv presente
  if [[ -f "${sandbox_dir}/bin/activate" ]]; then
    pass "Virtualenv presente: $sandbox_dir"
  else
    fail "Virtualenv presente" "${sandbox_dir}/bin/activate non trovato"
    return
  fi

  # Python nella sandbox
  local sandbox_python="${sandbox_dir}/bin/python3"
  if [[ -x "$sandbox_python" ]]; then
    local ver
    ver=$("$sandbox_python" --version 2>&1)
    pass "Python sandbox: $ver"
  else
    fail "Python sandbox" "$sandbox_python non eseguibile"
    return
  fi

  # Librerie chiave installate nella sandbox
  local py_packages=(
    "odoo"
    "psycopg2"
    "Pillow"
    "lxml"
    "werkzeug"
    "requests"
    "cryptography"
    "PyYAML"
  )

  local all_py_ok=true
  for pkg in "${py_packages[@]}"; do
    if "$sandbox_python" -c "import importlib; importlib.import_module('${pkg,,}')" &>/dev/null \
       || "$sandbox_python" -m pip show "$pkg" &>/dev/null; then
      verbose "Libreria Python: $pkg ✔"
    else
      fail "Libreria Python: $pkg" "non trovata nella sandbox"
      all_py_ok=false
    fi
  done
  [[ "$all_py_ok" == "true" ]] && pass "Librerie Python essenziali presenti"
}

# ---------------------------------------------------------------------------
# GRUPPO 6 — File di configurazione
# ---------------------------------------------------------------------------
check_config() {
  section "File di configurazione"

  if [[ ! -f "$ODOO_CONF" ]]; then
    fail "File conf esiste" "$ODOO_CONF non trovato"
    return
  fi
  pass "File conf esiste: $ODOO_CONF"

  # Sezione [options]
  if grep -q '^\[options\]' "$ODOO_CONF"; then
    pass "Sezione [options] presente"
  else
    fail "Sezione [options]" "non trovata in $ODOO_CONF"
  fi

  # Chiavi obbligatorie
  local required_keys=(
    "db_user"
    "addons_path"
    "http_port"
    "admin_passwd"
    "logfile"
  )

  for key in "${required_keys[@]}"; do
    if grep -qE "^${key}\s*=" "$ODOO_CONF"; then
      local val
      val=$(grep -E "^${key}\s*=" "$ODOO_CONF" | head -1 | cut -d= -f2- | sed 's/^ *//')
      pass "Chiave conf '$key': $val"
    else
      fail "Chiave conf '$key'" "non trovata in $ODOO_CONF"
    fi
  done

  # addons_path punta a directory esistenti
  local addons_line
  addons_line=$(grep -E "^addons_path\s*=" "$ODOO_CONF" | head -1 | cut -d= -f2- | sed 's/^ *//')
  IFS=',' read -ra addons_dirs <<< "$addons_line"
  for adir in "${addons_dirs[@]}"; do
    adir="${adir// /}"
    if [[ -d "$adir" ]]; then
      pass "addons_path dir esiste: $adir"
    else
      fail "addons_path dir esiste" "'$adir' non trovata"
    fi
  done

  # Log directory scrivibile
  local logfile_val
  logfile_val=$(grep -E "^logfile\s*=" "$ODOO_CONF" | head -1 | cut -d= -f2- | sed 's/^ *//')
  local logdir
  logdir=$(dirname "$logfile_val")
  if [[ -d "$logdir" && -w "$logdir" ]]; then
    pass "Log directory scrivibile: $logdir"
  else
    fail "Log directory scrivibile" "'$logdir' non esiste o non è scrivibile"
  fi

  # Porta non privilegiata (> 1024) o con capabilities
  local port_val
  port_val=$(grep -E "^http_port\s*=" "$ODOO_CONF" | head -1 | cut -d= -f2- | sed 's/^ *//' | tr -d ' ')
  if [[ "$port_val" =~ ^[0-9]+$ && $port_val -gt 1024 ]]; then
    pass "http_port non privilegiata: $port_val"
  else
    warn "http_port '$port_val' < 1024: richiede CAP_NET_BIND_SERVICE o root"
    pass "http_port configurata: $port_val"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 7 — Servizio systemd
# ---------------------------------------------------------------------------
check_systemd() {
  section "Servizio systemd"

  local service_name="odoo${ODOO_VERSION}"
  local service_file="/etc/systemd/system/${service_name}.service"

  # File service esiste
  if [[ -f "$service_file" ]]; then
    pass "File service esiste: $service_file"
  else
    fail "File service esiste" "$service_file non trovato"
    return
  fi

  # Sezioni obbligatorie
  for section_tag in "[Unit]" "[Service]" "[Install]"; do
    if grep -qF "$section_tag" "$service_file"; then
      pass "Sezione service '$section_tag' presente"
    else
      fail "Sezione service '$section_tag'" "non trovata in $service_file"
    fi
  done

  # User= impostato correttamente
  if grep -qE "^User=${ODOO_USER}$" "$service_file"; then
    pass "User= nel service: $ODOO_USER"
  else
    fail "User= nel service" "atteso 'User=${ODOO_USER}'"
  fi

  # Dipendenza da postgresql
  if grep -q "postgresql.service" "$service_file"; then
    pass "Dipendenza postgresql.service dichiarata"
  else
    fail "Dipendenza postgresql.service" "non trovata nel file service"
  fi

  # Servizio abilitato all'avvio
  if systemctl is-enabled --quiet "${service_name}" 2>/dev/null; then
    pass "Servizio ${service_name} abilitato (enable)"
  else
    fail "Servizio ${service_name} abilitato" "systemctl enable non eseguito"
  fi

  # Servizio attivo
  if systemctl is-active --quiet "${service_name}"; then
    pass "Servizio ${service_name} attivo (running)"
  else
    local status
    status=$(systemctl is-active "${service_name}" 2>/dev/null || echo "unknown")
    fail "Servizio ${service_name} attivo" "stato: $status"
  fi

  # Restart= configurato
  if grep -qE "^Restart=" "$service_file"; then
    pass "Restart policy configurata"
  else
    warn "Restart= non trovato nel service — nessun auto-restart in caso di crash"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 8 — Connettività HTTP Odoo
# ---------------------------------------------------------------------------
check_connectivity() {
  section "Connettività HTTP"

  local base_url="http://localhost:${ODOO_PORT}"

  # curl disponibile
  if ! command -v curl &>/dev/null; then
    skip "Test connettività HTTP" "curl non disponibile"
    return
  fi

  # Odoo risponde
  local http_code
  http_code=$(curl -s -o /dev/null -w '%{http_code}' \
    --max-time 10 "${base_url}/web/database/selector" 2>/dev/null || echo "000")

  if [[ "$http_code" == "200" || "$http_code" == "303" ]]; then
    pass "Odoo risponde su porta ${ODOO_PORT} (HTTP $http_code)"
  elif [[ "$http_code" == "000" ]]; then
    fail "Odoo risponde su porta ${ODOO_PORT}" "timeout o connessione rifiutata"
  else
    warn "Odoo risponde con HTTP $http_code — potrebbe essere normale in fase di init"
    pass "Odoo raggiungibile su porta ${ODOO_PORT} (HTTP $http_code)"
  fi

  # Endpoint jsonrpc
  local rpc_code
  rpc_code=$(curl -s -o /dev/null -w '%{http_code}' \
    --max-time 10 \
    -X POST "${base_url}/web/dataset/call_kw" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"call","id":1,"params":{}}' 2>/dev/null || echo "000")

  if [[ "$rpc_code" =~ ^(200|400|404)$ ]]; then
    pass "Endpoint JSON-RPC raggiungibile (HTTP $rpc_code)"
  else
    fail "Endpoint JSON-RPC" "HTTP $rpc_code (atteso 200/400/404)"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 9 — Nginx (opzionale)
# ---------------------------------------------------------------------------
check_nginx() {
  section "Nginx (reverse proxy)"

  if ! command -v nginx &>/dev/null; then
    skip "Nginx installato" "non presente — saltando tutti i check nginx"
    return
  fi
  pass "Nginx installato"

  if systemctl is-active --quiet nginx; then
    pass "Nginx attivo"
  else
    fail "Nginx attivo" "systemctl is-active nginx fallito"
  fi

  # Configurazione sintatticamente valida
  if nginx -t &>/dev/null; then
    pass "Configurazione nginx valida (nginx -t)"
  else
    local err
    err=$(nginx -t 2>&1 | tail -1)
    fail "Configurazione nginx valida" "$err"
  fi

  # Proxy_pass verso Odoo presente
  if grep -r "proxy_pass.*${ODOO_PORT}" /etc/nginx/sites-enabled/ &>/dev/null \
    || grep -r "proxy_pass.*${ODOO_PORT}" /etc/nginx/conf.d/ &>/dev/null; then
    pass "proxy_pass verso porta ${ODOO_PORT} configurato"
  else
    fail "proxy_pass verso porta ${ODOO_PORT}" "non trovato in sites-enabled/ o conf.d/"
  fi
}

# ---------------------------------------------------------------------------
# GRUPPO 10 — Sicurezza e best practice
# ---------------------------------------------------------------------------
check_security() {
  section "Sicurezza e best practice"

  # odoo NON deve essere in sudoers
  if ! sudo -l -U "$ODOO_USER" 2>/dev/null | grep -q "NOPASSWD"; then
    pass "Utente '$ODOO_USER' non ha sudo NOPASSWD"
  else
    fail "Utente '$ODOO_USER' non ha sudo NOPASSWD" "rimuovere da sudoers per sicurezza"
  fi

  # admin_passwd non deve essere 'admin' in produzione
  if [[ -f "$ODOO_CONF" ]]; then
    local admin_pass
    admin_pass=$(grep -E "^admin_passwd\s*=" "$ODOO_CONF" | head -1 | cut -d= -f2- | sed 's/^ *//')
    if [[ "$admin_pass" != "admin" && -n "$admin_pass" ]]; then
      pass "admin_passwd non è il valore di default 'admin'"
    else
      warn "admin_passwd è 'admin' — cambiare prima di andare in produzione"
    fi
  fi

  # Porta 8069 non esposta pubblicamente (se UFW attivo)
  if command -v ufw &>/dev/null && ufw status 2>/dev/null | grep -q "Status: active"; then
    if ufw status 2>/dev/null | grep -q "${ODOO_PORT}"; then
      warn "Porta ${ODOO_PORT} aperta nel firewall — in produzione usare solo nginx su 80/443"
    else
      pass "Porta ${ODOO_PORT} non esposta direttamente nel firewall"
    fi
  else
    skip "Verifica firewall UFW" "UFW non attivo o non installato"
  fi

  # File di configurazione non leggibile da tutti
  if [[ -f "$ODOO_CONF" ]]; then
    local perms
    perms=$(stat -c '%a' "$ODOO_CONF")
    if [[ "$perms" == "640" || "$perms" == "600" ]]; then
      pass "Permessi odoo.conf: $perms (sicuri)"
    else
      warn "Permessi odoo.conf: $perms — consigliato 640 (chmod 640 $ODOO_CONF)"
    fi
  fi

  # Log directory scrivibile solo da odoo
  if [[ -d /var/log/odoo ]]; then
    local log_owner
    log_owner=$(stat -c '%U' /var/log/odoo)
    if [[ "$log_owner" == "$ODOO_USER" ]]; then
      pass "Log directory proprietà: $log_owner"
    else
      fail "Log directory proprietà" "atteso '$ODOO_USER', trovato '$log_owner'"
    fi
  fi
}

# ---------------------------------------------------------------------------
# Report finale
# ---------------------------------------------------------------------------
print_summary() {
  echo ""
  echo -e "${BOLD}$(printf '═%.0s' $(seq 1 60))${RESET}"
  echo -e "${BOLD}  RIEPILOGO TEST — Odoo ${ODOO_VERSION} su $(hostname)${RESET}"
  echo -e "${BOLD}$(printf '═%.0s' $(seq 1 60))${RESET}"
  echo ""
  echo -e "  Test eseguiti : ${BOLD}$TESTS_RUN${RESET}"
  echo -e "  ${GREEN}Superati       : $TESTS_PASSED${RESET}"
  echo -e "  ${RED}Falliti        : $TESTS_FAILED${RESET}"
  echo -e "  ${YELLOW}Saltati        : $TESTS_SKIPPED${RESET}"
  echo ""

  if [[ ${#FAILED_TESTS[@]} -gt 0 ]]; then
    echo -e "${RED}${BOLD}  Test falliti:${RESET}"
    for t in "${FAILED_TESTS[@]}"; do
      echo -e "  ${RED}•${RESET} $t"
    done
    echo ""
  fi

  if [[ $TESTS_FAILED -eq 0 ]]; then
    echo -e "${GREEN}${BOLD}  ✔  Installazione verificata con successo.${RESET}"
  else
    echo -e "${RED}${BOLD}  ✘  Installazione incompleta — correggere i test falliti.${RESET}"
    echo -e "${DIM}  Per dettagli: sudo bash $0 --verbose${RESET}"
  fi
  echo ""
}

# ---------------------------------------------------------------------------
# Entrypoint
# ---------------------------------------------------------------------------
main() {
  parse_args "$@"

  echo ""
  echo -e "${BOLD}${CYAN}╔══════════════════════════════════════════════════════════╗${RESET}"
  echo -e "${BOLD}${CYAN}║   odoo-autoinstaller — check_install.sh                  ║${RESET}"
  echo -e "${BOLD}${CYAN}║   Verifica installazione Odoo ${ODOO_VERSION} — $(date '+%Y-%m-%d %H:%M')       ║${RESET}"
  echo -e "${BOLD}${CYAN}╚══════════════════════════════════════════════════════════╝${RESET}"

  check_system
  check_dependencies
  check_user_and_dirs
  check_postgres
  check_odoo_install
  check_config
  check_systemd
  check_connectivity
  check_nginx
  check_security

  print_summary

  [[ $TESTS_FAILED -eq 0 ]]
}

main "$@"