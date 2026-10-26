#!/bin/sh

# SPDX-FileCopyrightText: 2026  John Hamilton
# SPDX-License-Identifier: GPL-3.0-or-later

# Builds the prebuilt tarball of Dismas, dismas-<version>-<arch>.tar.xz,
# which runs from wherever it's unpacked: its folder is laid out like an
# installed prefix, and Dismas looks for its data next to its binary
# (data_paths in src/main.rs). The Release workflow attaches it to each
# release.
#
# Usage: build-aux/make-tarball.sh [BUILD_DIR [OUT_DIR]]
# BUILD_DIR defaults to _build-tarball, and OUT_DIR to BUILD_DIR.
#
# The binary links the system's GTK, libadwaita and GStreamer, and its
# glibc, so it only runs on a glibc at least as new as the one it needs.
# The build fails if that's newer than MAX_GLIBC (2.41 by default), the
# oldest glibc of the distributions with GNOME 49's libraries.

set -eu
export LC_ALL=C

src=$(cd "$(dirname "$0")/.." && pwd -P)
build=${1:-$src/_build-tarball}
out=${2:-$build}
max_glibc=${MAX_GLIBC:-2.41}

# Setting up a build copies its config.rs into the source tree, over the
# one of the build folder in use there, so that one is put back after
config=$src/src/config.rs
saved=
if [ -f "$config" ]; then
    saved=$(mktemp)
    cp "$config" "$saved"
fi
restore_config() {
    if [ -n "$saved" ]; then
        cp "$saved" "$config"
        rm -f "$saved"
    fi
}
trap restore_config EXIT

# Kept out of git, as Meson does for a build folder it makes itself
mkdir -p "$build"
[ -f "$build/.gitignore" ] || echo '*' >"$build/.gitignore"

if [ -f "$build/meson-info/meson-info.json" ]; then
    meson setup --reconfigure "$build" "$src" --prefix=/usr/local -Dprofile=default
else
    meson setup "$build" "$src" --prefix=/usr/local -Dprofile=default
fi
meson compile -C "$build"

stage=$build/stage
rm -rf "$stage"
meson install -C "$build" --no-rebuild --destdir "$stage"

version=$(meson introspect --projectinfo "$build" | sed -n 's/.*"version": *"\([^"]*\)".*/\1/p')
name=dismas-$version-$(uname -m)
dir=$build/$name
rm -rf "$dir"
mkdir -p "$dir"
cp -a "$stage/usr/local/bin" "$stage/usr/local/share" "$dir/"

# The schema compiled in a folder of its own, which Dismas adds to
# GSETTINGS_SCHEMA_DIR, rather than in glib-2.0/schemas, where a compiled
# schema unpacked into ~/.local would replace the other apps' ones
schemas=$dir/share/dismas/schemas
mkdir -p "$schemas"
mv "$dir"/share/glib-2.0/schemas/*.gschema.xml "$schemas/"
rm -r "$dir/share/glib-2.0"
glib-compile-schemas --strict "$schemas"
# Caches made for the staged install, if any, which only fit that one
rm -f "$dir/share/icons/hicolor/icon-theme.cache" "$dir/share/applications/mimeinfo.cache"

cp "$src/build-aux/install.sh" "$dir/install.sh"
chmod +x "$dir/install.sh"
cp "$src/LICENSES/GPL-3.0-or-later.txt" "$dir/COPYING"

glibc=$(objdump -T "$dir/bin/dismas" | grep -o 'GLIBC_[0-9.]*' | sed 's/^GLIBC_//' | sort -Vu | tail -n 1)
echo "The binary needs glibc $glibc or later"
if [ "$(printf '%s\n%s\n' "$glibc" "$max_glibc" | sort -V | tail -n 1)" != "$max_glibc" ]; then
    echo "error: that's newer than glibc $max_glibc; build it on an older distribution" >&2
    exit 1
fi

cat >"$dir/README.txt" <<EOF
Dismas $version, a music player for GNOME, built for $(uname -m) Linux

Dismas runs from this folder, wherever it is:

    bin/dismas

To add it to the menu, run ./install.sh, which links it from ~/.local, and
./install.sh --uninstall to take it out again. To update, unpack the new
version and run its install.sh; to remove Dismas, uninstall it and delete
this folder.

It needs, from your distribution:
- glibc $glibc or later
- GTK 4.20 or later and libadwaita 1.8 or later, as in GNOME 49 or later
- GStreamer 1.20 or later, with the base, good and bad plugins
- the glycin loaders and bwrap, to read the covers
- SQLite, libseccomp and fontconfig

It shares its settings, library and playlists with any other Dismas you
install for your account, as they're the same app.

For a debug log, to add to a bug report, run it from a terminal with:

    RUST_BACKTRACE=1 RUST_LOG=dismas=debug bin/dismas

Source code, other ways to install it, and bug reports:
https://github.com/dachshund7/dismas

Dismas is free software, released under the GNU General Public License,
version 3 or later; see COPYING.
EOF

mkdir -p "$out"
tar -C "$build" --sort=name --owner=0 --group=0 --numeric-owner -cJf "$out/$name.tar.xz" "$name"
echo "$out/$name.tar.xz"
