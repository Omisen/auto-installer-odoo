# =============================================================================
# templates/nginx.conf.tpl — Reverse proxy Nginx per Odoo 18
# Placeholder sostituiti da lib/nginx.sh tramite sed:
#   {{NGINX_SERVER_NAME}}   nome di dominio o IP (es. odoo.example.com)
#   {{ODOO_PORT}}           porta locale di Odoo (default 8069)
#   {{NGINX_CLIENT_MAX}}    dimensione massima body upload (default 100m)
#   {{NGINX_CERT_PATH}}     percorso certificato TLS
#   {{NGINX_KEY_PATH}}      percorso chiave privata TLS
# =============================================================================

# ── Redirect HTTP → HTTPS (attivo solo se NGINX_ENABLE_SSL=true) ─────────────
# Se SSL è disabilitato questo blocco non viene generato nel file finale;
# il template viene sempre scritto completo e nginx.sh lascia o rimuove
# questo blocco via sed quando NGINX_ENABLE_SSL != true.
# Per semplicità mantenere entrambi i blocchi: il secondo server{} su 443
# sarà ignorato da Nginx se il certificato non esiste — l'importante è che
# nginx -t passi. In produzione SSL usare Certbot o fornire cert validi.
# =============================================================================

# ── Upstream Odoo ─────────────────────────────────────────────────────────────
upstream odoo {
    server 127.0.0.1:{{ODOO_PORT}};
    keepalive 16;
}

upstream odoo-longpolling {
    server 127.0.0.1:8072;
}

# ── HTTP (porta 80) ───────────────────────────────────────────────────────────
server {
    listen 80;
    listen [::]:80;
    server_name {{NGINX_SERVER_NAME}};

    # Se SSL è attivo redirige tutto il traffico HTTP su HTTPS.
    # Commentare o rimuovere questo blocco per servire Odoo solo in HTTP.
    # return 301 https://$host$request_uri;

    access_log  /var/log/nginx/odoo18.access.log;
    error_log   /var/log/nginx/odoo18.error.log;

    # Dimensione massima dei file caricati (fatture, allegati, ecc.)
    client_max_body_size {{NGINX_CLIENT_MAX}};

    # Header di sicurezza
    add_header X-Frame-Options         SAMEORIGIN;
    add_header X-Content-Type-Options  nosniff;
    add_header X-XSS-Protection        "1; mode=block";
    add_header Referrer-Policy         "strict-origin-when-cross-origin";

    # ── Longpolling (notifiche real-time, bus) ────────────────────────────────
    location /web/websocket {
        proxy_pass         http://odoo-longpolling;
        proxy_http_version 1.1;
        proxy_set_header   Upgrade    $http_upgrade;
        proxy_set_header   Connection "upgrade";
        proxy_set_header   Host       $host;
        proxy_set_header   X-Real-IP  $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    location /longpolling {
        proxy_pass         http://odoo-longpolling;
        proxy_http_version 1.1;
        proxy_set_header   Host               $host;
        proxy_set_header   X-Real-IP          $remote_addr;
        proxy_set_header   X-Forwarded-For    $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto  $scheme;
        proxy_read_timeout 3600s;
        proxy_send_timeout 3600s;
    }

    # ── Contenuto statico con cache aggressiva ────────────────────────────────
    location ~* /web/static/ {
        proxy_pass         http://odoo;
        proxy_cache_valid  200 90d;
        proxy_buffering    on;
        expires            864000;
        add_header         Cache-Control "public, immutable";
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;
    }

    # ── Proxy principale ──────────────────────────────────────────────────────
    location / {
        proxy_pass         http://odoo;
        proxy_http_version 1.1;
        proxy_set_header   Host              $http_host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;

        # Timeout generosi per operazioni pesanti (importazioni, report)
        proxy_read_timeout  720s;
        proxy_send_timeout  720s;
        proxy_connect_timeout 30s;

        # Disabilita il buffering per lo streaming (es. download di backup)
        proxy_buffering off;

        # Necessario per proxy_pass con header chunked
        proxy_set_header   Connection "";
    }

    # ── Blocca l'accesso diretto ai file di sistema Odoo ─────────────────────
    location ~* \.(py|pyc|cfg|conf)$ {
        deny all;
    }
}

# ── HTTPS (porta 443) — attivare solo con certificati validi ──────────────────
# server {
#     listen 443 ssl http2;
#     listen [::]:443 ssl http2;
#     server_name {{NGINX_SERVER_NAME}};
#
#     ssl_certificate     {{NGINX_CERT_PATH}};
#     ssl_certificate_key {{NGINX_KEY_PATH}};
#
#     ssl_protocols             TLSv1.2 TLSv1.3;
#     ssl_ciphers               ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384;
#     ssl_prefer_server_ciphers off;
#     ssl_session_cache         shared:SSL:10m;
#     ssl_session_timeout       1d;
#     ssl_session_tickets       off;
#     ssl_stapling              on;
#     ssl_stapling_verify       on;
#
#     # (stesse location del blocco HTTP sopra)
# }
