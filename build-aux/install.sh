#!/bin/sh

# SPDX-FileCopyrightText: 2026  John Hamilton
# SPDX-License-Identifier: GPL-3.0-or-later

# Adds the prebuilt Dismas in this folder to the menu of the current user,
# or with --uninstall takes it out again. Dismas runs from this folder, so
# the files written elsewhere only point to it:
#   ~/.local/bin/dismas, a link to bin/dismas
#   the desktop file and the D-Bus service, which start that link
#   links to the icons, which the menu and the tray look up by name
# To update, unpack the new version and run its install.sh.

set -eu

app_id=io.github.dachshund7.Dismas
mark="# Written by the install.sh of a prebuilt Dismas"

here=$(cd "$(dirname "$0")" && pwd -P)
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
bin_dir=$HOME/.local/bin
link=$bin_dir/dismas
desktop=$data_home/applications/$app_id.desktop
service=$data_home/dbus-1/services/$app_id.service
scalable=icons/hicolor/scalable/apps
symbolic=icons/hicolor/symbolic/apps
icons="$scalable/$app_id.svg $symbolic/$app_id-symbolic.svg"

usage() {
    echo "Usage: $0 [--uninstall]"
}

# Whether a file can be replaced or removed: a link into a prebuilt
# Dismas (a folder with an install.sh next to bin/dismas), a link to
# nothing, or a file this script wrote. Anything else belongs to another
# install of Dismas, such as one built from source in ~/.local, which has
# the same files
is_ours() {
    if [ -L "$1" ]; then
        target=$(readlink "$1")
        case $target in
        */bin/dismas) prefix=${target%/bin/dismas} ;;
        */share/icons/*) prefix=${target%%/share/icons/*} ;;
        *) prefix= ;;
        esac
        [ ! -e "$1" ] || { [ -n "$prefix" ] && [ -f "$prefix/install.sh" ] && [ -f "$prefix/bin/dismas" ]; }
    elif [ -f "$1" ]; then
        grep -qxF "$mark" "$1"
    else
        [ ! -e "$1" ]
    fi
}

# Every file this script writes
targets() {
    echo "$link"
    echo "$desktop"
    echo "$service"
    for icon in $icons; do
        echo "$data_home/$icon"
    done
}

check_targets() {
    targets | while IFS= read -r file; do
        if ! is_ours "$file"; then
            echo "$file belongs to another install of Dismas, so it's left as it is." >&2
            echo "Uninstall that one first, then run $0 again." >&2
            exit 1
        fi
    done
}

update_menu() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database -q "$data_home/applications" 2>/dev/null || true
    fi
}

install() {
    if [ ! -x "$here/bin/dismas" ]; then
        echo "$here/bin/dismas isn't there: run this script from the folder it came in." >&2
        exit 1
    fi
    # The desktop file and the D-Bus service start the link by its path,
    # which they would need quoted with such characters
    case $link in
    *[!A-Za-z0-9._/+@-]*)
        echo "The path $link has characters the menu can't start Dismas from." >&2
        exit 1
        ;;
    esac
    # Its loop runs in a subshell, which can only end itself
    check_targets || exit 1

    mkdir -p "$bin_dir" "$(dirname "$desktop")" "$(dirname "$service")"
    ln -sfn "$here/bin/dismas" "$link"

    {
        echo "$mark"
        while IFS= read -r line; do
            case $line in
            Exec=*) echo "Exec=$link %U" ;;
            TryExec=*) echo "TryExec=$link" ;;
            *) printf '%s\n' "$line" ;;
            esac
        done <"$here/share/applications/$app_id.desktop"
    } >"$desktop.tmp"
    mv "$desktop.tmp" "$desktop"

    cat >"$service.tmp" <<EOF
$mark
[D-BUS Service]
Name=$app_id
Exec=$link --gapplication-service
EOF
    mv "$service.tmp" "$service"

    for icon in $icons; do
        mkdir -p "$(dirname "$data_home/$icon")"
        ln -sfn "$here/share/$icon" "$data_home/$icon"
    done

    update_menu
    echo "Dismas is in the menu, and runs from $here."
    case ":$PATH:" in
    *":$bin_dir:"*) echo "It also runs as dismas in a terminal." ;;
    *) echo "To run it as dismas in a terminal, add $bin_dir to your PATH." ;;
    esac
}

uninstall() {
    targets | while IFS= read -r file; do
        if { [ -e "$file" ] || [ -L "$file" ]; } && is_ours "$file"; then
            rm -f "$file"
        fi
    done
    update_menu
    echo "Dismas is out of the menu. To remove it, delete $here."
}

case ${1-} in
"") install ;;
--uninstall) uninstall ;;
-h | --help) usage ;;
*)
    usage >&2
    exit 1
    ;;
esac
