# SPDX-FileCopyrightText: 2022  Emmanuele Bassi
# SPDX-License-Identifier: GPL-3.0-or-later

scalable_dir = 'hicolor/scalable/apps'
install_data(
  scalable_dir / ('@0@.svg').format(application_id),
  install_dir: get_option('datadir') / 'icons' / scalable_dir,
)

symbolic_dir = 'hicolor/symbolic/apps'
install_data(
  symbolic_dir / ('@0@-symbolic.svg').format(application_id),
  install_dir: get_option('datadir') / 'icons' / symbolic_dir,
)

# The tray icon has the app ID as its prefix, so Flatpak exports it to the
# host, where the tray looks it up
install_data(
  scalable_dir / 'io.github.dachshund7.Dismas-tray.svg',
  install_dir: get_option('datadir') / 'icons' / scalable_dir,
  rename: '@0@-tray.svg'.format(application_id),
)
