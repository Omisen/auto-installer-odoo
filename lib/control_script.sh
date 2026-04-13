#!/usr/bin/env bash
# =============================================================================
# lib/control_script.sh — Install Odoo control helper script
#
# Public functions:
#   install_odoo_control_script — installs the odoo helper command
#
# Depends on globals/functions from installer.sh:
#   - set -euo pipefail inherited from caller
# =============================================================================

set -euo pipefail

install_odoo_control_script() {
  # Rigorous: installer must run via sudo, so SUDO_USER must be set.
  if [[ -z "${SUDO_USER}" ]]; then
    error "install_odoo_control_script: SUDO_USER not set. This script must be run via sudo."
  fi

  local target_user="${SUDO_USER}"
  local target_home
  local control_script
  local scripts_dir
  local local_bin_dir
  local bashrc_file
  local path_export='export PATH="$HOME/.local/bin:$PATH"'


  local service_name
  if declare -F _unit_name >/dev/null 2>&1; then
    service_name="$(_unit_name)"
  else
    service_name="odoo${ODOO_VERSION_SHORT}"
  fi



  target_home="$(getent passwd "${target_user}" | cut -d: -f6)"
  if [[ -z "${target_home}" ]]; then
    error "Impossibile determinare la home per l'utente ${target_user}."
  fi

  scripts_dir="${target_home}/.scripts"
  control_script="${scripts_dir}/odoo.sh"
  local_bin_dir="${target_home}/.local/bin"
  bashrc_file="${target_home}/.bashrc"

  mkdir -p "${scripts_dir}" "${local_bin_dir}"



  {
    echo '#!/usr/bin/env bash'
    echo ''
    echo 'set -euo pipefail'
    echo ''
    echo "SERVICE_NAME=\"${service_name}\""
    echo "ODOO_OS_USER=\"${ODOO_USER}\""
    cat <<'EOF'


usage() {
  echo "Usage: odoo {start|stop|restart|dev|status}"
}

case "${1:-}" in
  start)
    sudo systemctl start odoo
    ;;
  stop)
    sudo systemctl stop odoo
    ;;
  restart)
    sudo systemctl restart odoo
    ;;
  dev)
    sudo systemctl stop "${SERVICE_NAME}"
    sudo su - "${ODOO_OS_USER}" -s /bin/bash
    ;;
  status)
    sudo systemctl status odoo
    ;;
  *)
    usage
    ;;
esac
EOF

  chmod +x "${control_script}"

  ln -sf "${control_script}" "${local_bin_dir}/odoo"

  if [[ ! -f "${bashrc_file}" ]]; then
    touch "${bashrc_file}"
  fi

  if ! grep -Fqx "${path_export}" "${bashrc_file}"; then
    echo "${path_export}" >> "${bashrc_file}"
    log "TIP: Esecuzione di: source ~/.bashrc per applicazione modifiche"
  fi

  if [[ "${EUID}" -eq 0 ]]; then
    chown "${target_user}:${target_user}" "${control_script}" "${bashrc_file}"
    chown -h "${target_user}:${target_user}" "${local_bin_dir}/odoo"
    chown "${target_user}:${target_user}" "${scripts_dir}" "${local_bin_dir}"
  fi

  log "Control script Odoo installato per l'utente ${target_user} in ${control_script}"
}