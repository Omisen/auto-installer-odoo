#!/bin/sh
# the rpm %post scriptlet — counterpart of debian/postinst.
#
# creates the short alias `vok` -> `invok` as a RELATIVE symlink, valid under an
# alternative root, and not as a second binary.
#
# what is not ours is not overwritten: an existing regular file at that path
# belongs to somebody else, so warn and give up the alias.
#
# why a file and not a string inside Cargo.toml: the logic is identical to the
# .deb's, and two copies in two formats diverge with nothing to say so. as files,
# a test can compare them.
#
# the GUARDS do diverge by necessity: the two packaging conventions pass
# different arguments. here no check is needed — %post runs on install and on
# upgrade, and we want the alias in both.

if [ -e /usr/bin/vok ] && [ ! -L /usr/bin/vok ]; then
    echo "invok: /usr/bin/vok esiste e non è un collegamento simbolico." >&2
    echo "invok: alias 'vok' NON creato; usa il comando 'invok'." >&2
else
    ln -sfn invok /usr/bin/vok
fi

exit 0
