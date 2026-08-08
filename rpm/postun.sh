#!/bin/sh
# Scriptlet %postun del .rpm — controparte di debian/postrm.
#
# Rimuove l'alias `vok` a due condizioni, non una:
#   1. solo alla disinstallazione VERA. In rpm il primo parametro di %postun è
#      il numero di installazioni RIMASTE: `0` = rimozione, `1` = aggiornamento.
#      Durante un aggiornamento rpm esegue comunque il %postun della versione
#      uscente, e cancellare lì lascerebbe l'utente senza alias — è lo stesso
#      difetto che nel `.deb` si evita escludendo `upgrade`.
#   2. solo se il link punta ancora a `invok`. Se un amministratore l'ha
#      ripuntato altrove, quel link non è più nostro e non è nostro da rimuovere.
#
# È la stessa logica di debian/postrm con la guardia scritta nella convenzione
# di rpm. Vedi la nota in rpm/post.sh sul perché sta in un file.

if [ "$1" = "0" ] && [ -L /usr/bin/vok ] && [ "$(readlink /usr/bin/vok)" = "invok" ]; then
    rm -f /usr/bin/vok
fi

exit 0
