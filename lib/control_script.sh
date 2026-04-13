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
  local control_script
  local scripts_dir
  local local_bin_dir
  local bashrc_file
  local path_export='export PATH="$HOME/.local/bin:$PATH"'

  # L'installer gira come root: installiamo il comando in /usr/local/bin
  # così è disponibile a tutti gli utenti senza toccare /root/.bashrc.
  if [[ "${EUID}" -eq 0 ]]; then
    control_script="/usr/local/bin/odoo"
  else
    scripts_dir="${HOME}/.scripts"
    control_script="${scripts_dir}/odoo.sh"
    local_bin_dir="${HOME}/.local/bin"
    bashrc_file="${HOME}/.bashrc"

    mkdir -p "${scripts_dir}"
  fi

  cat <<'EOF' > "${control_script}"
#!/usr/bin/env bash

set -euo pipefail

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
    sudo systemctl stop odoo
    sudo su - odoo -s /bin/bash
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

  if [[ "${EUID}" -eq 0 ]]; then
    log "Control script Odoo installato in ${control_script}"
    return 0
  fi

  if [[ ! -d "${local_bin_dir}" ]]; then
    mkdir -p "${local_bin_dir}"
  fi

  ln -sf "${control_script}" "${local_bin_dir}/odoo"

  if [[ ! -f "${bashrc_file}" ]]; then
    touch "${bashrc_file}"
  fi

  if ! grep -Fqx "${path_export}" "${bashrc_file}"; then
    echo "${path_export}" >> "${bashrc_file}"
    log "TIP: Esecuzione di: source ~/.bashrc per applicazione modifiche"
  fi
}