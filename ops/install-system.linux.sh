#!/bin/sh
# Written by `spi install` from spi.toml. Read it, then run it as root:
#
#     sudo sh install-system.sh
#
# It creates the service account and the folders, then installs the binaries,
# settings, env files and service definitions that are next to it. It reads them
# from this script's own folder, so the folder can be copied to another machine
# first. It starts nothing. Running it again is safe: it replaces what spi writes
# and keeps what is yours (the *.secret.env and *-health.conf files).
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "run this as root: sudo sh $0" >&2
    exit 1
fi
here=$(cd "$(dirname "$0")" && pwd)

{{settings}}

# put MODE OWNER GROUP SOURCE DEST: install a file, replacing what is there.
put() {
    install -m "$1" -o "$2" -g "$3" "$4" "$5"
    echo "installed $5"
}

# keep MODE OWNER GROUP SOURCE DEST: install a file unless it is already there.
keep() {
    if [ -e "$5" ]; then
        echo "kept $5 (it is yours: spi does not overwrite it)"
    else
        put "$1" "$2" "$3" "$4" "$5"
    fi
}

# need PATH NAME: say so if a binary is not where the service definitions run it.
need() {
    [ -x "$1" ] || echo "WARNING: $1 is missing: install $2 there before starting the services" >&2
}

# The account: no login, no password, owns the data and the logs.
if ! getent group "$group" >/dev/null 2>&1; then
    groupadd --system "$group"
    echo "created the group $group"
fi
if ! id -u "$user" >/dev/null 2>&1; then
    useradd --system --gid "$group" --home-dir "$data_dir" --no-create-home --shell /usr/sbin/nologin "$user"
    echo "created the user $user"
fi

# The folders.
install -d -m 0750 -o root -g "$group" "$etc_dir"
install -d -m 0750 -o "$user" -g "$group" "$data_dir" "$log_dir"
{{folders}}

{{installs}}

if [ -d /etc/logrotate.d ]; then
    put 0644 root root "$here/logrotate-sp-mint" /etc/logrotate.d/sp-mint
fi

systemctl daemon-reload
echo "done. Nothing is started: start the services as spi install printed."
