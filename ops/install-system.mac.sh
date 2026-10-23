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

# needfile PATH WHAT: say so if a file that is yours to provide is not there.
needfile() {
    [ -e "$1" ] || echo "WARNING: $1 is missing: $2" >&2
}

# free_id: the first id from 399 down (the range kept for service accounts)
# that no user and no group has.
free_id() {
    i=399
    while [ "$i" -ge 200 ]; do
        if ! dscl . -list /Users UniqueID | awk '{print $2}' | grep -qx "$i" &&
            ! dscl . -list /Groups PrimaryGroupID | awk '{print $2}' | grep -qx "$i"; then
            echo "$i"
            return 0
        fi
        i=$((i - 1))
    done
    echo "no free id from 399 down to 200 for the service account" >&2
    return 1
}

# The account: no login, no password, hidden, owns the data and the logs.
if ! dscl . -read "/Groups/$group" >/dev/null 2>&1; then
    new_gid=$(free_id)
    dscl . -create "/Groups/$group"
    dscl . -create "/Groups/$group" PrimaryGroupID "$new_gid"
    echo "created the group $group ($new_gid)"
fi
gid=$(dscl . -read "/Groups/$group" PrimaryGroupID | awk '{print $2}')
if ! dscl . -read "/Users/$user" >/dev/null 2>&1; then
    new_uid=$(free_id)
    dscl . -create "/Users/$user"
    dscl . -create "/Users/$user" UniqueID "$new_uid"
    dscl . -create "/Users/$user" PrimaryGroupID "$gid"
    dscl . -create "/Users/$user" UserShell /usr/bin/false
    dscl . -create "/Users/$user" NFSHomeDirectory /var/empty
    dscl . -create "/Users/$user" RealName "Service Points mints"
    dscl . -create "/Users/$user" IsHidden 1
    echo "created the user $user ($new_uid)"
fi

# The folders.
install -d -m 0750 -o root -g "$group" "$etc_dir"
install -d -m 0750 -o "$user" -g "$group" "$data_dir" "$log_dir"
{{folders}}

# The folder `spi` writes the SPI wallet's balance in, for psp-mint's burn
# threshold ([psp] burn): the group writes it (spi runs as a member), the service
# user reads it. The setgid bit keeps the files in the group where it applies.
install -d -m 0770 -o "$user" -g "$group" "$data_dir/spi-share"
chmod 2770 "$data_dir/spi-share"

{{installs}}

echo "done. Nothing is started: start the services as spi install printed."
