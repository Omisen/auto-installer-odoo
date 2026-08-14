#!/bin/sh
# the rpm %postun scriptlet — counterpart of debian/postrm.
#
# removes the alias under two conditions, not one:
#   1. only on a REAL uninstall. rpm passes %postun the number of installations
#      LEFT: zero means removal, one means upgrade. rpm runs the outgoing
#      version's %postun during an upgrade too, and deleting there would leave
#      the user without the alias.
#   2. only if the link still points at our binary; repointed elsewhere it is no
#      longer ours to remove.
#
# the same logic as debian/postrm with the guard written in rpm's convention.
# see rpm/post.sh for why this lives in a file.

if [ "$1" = "0" ] && [ -L /usr/bin/vok ] && [ "$(readlink /usr/bin/vok)" = "invok" ]; then
    rm -f /usr/bin/vok
fi

exit 0
