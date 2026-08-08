#!/bin/sh
# Scriptlet %post del .rpm — controparte di debian/postinst.
#
# Crea l'alias breve `vok` → `invok`, come symlink RELATIVO (resta valido sotto
# una root alternativa) e non come secondo binario.
#
# Non si sovrascrive ciò che non è nostro: se `/usr/bin/vok` esiste ed è un file
# regolare appartiene a qualcun altro, si avvisa e si rinuncia all'alias. È la
# stessa regola che l'installer applica agli artefatti del cliente.
#
# Perché è un file e non una stringa dentro Cargo.toml: la logica è identica a
# quella del `.deb`, e due copie in due formati diversi — una in `debian/`, una
# in un manifesto TOML — sono due cose che divergono senza che nulla lo dica. In
# file, un test può confrontarle (`the_deb_and_the_rpm_install_the_same_alias`).
#
# Le GUARDIE invece divergono per forza, e non è una svista: deb e rpm passano
# argomenti diversi al primo parametro. Qui non serve alcun controllo su `$1` —
# %post gira sia all'installazione sia all'aggiornamento, e in entrambi i casi
# vogliamo l'alias.

if [ -e /usr/bin/vok ] && [ ! -L /usr/bin/vok ]; then
    echo "invok: /usr/bin/vok esiste e non è un collegamento simbolico." >&2
    echo "invok: alias 'vok' NON creato; usa il comando 'invok'." >&2
else
    ln -sfn invok /usr/bin/vok
fi

exit 0
