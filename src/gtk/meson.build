# SPDX-FileCopyrightText: 2025  Emmanuele Bassi
# SPDX-License-Identifier: GPL-3.0-or-later

blueprints = custom_target('blueprints',
  input: files(
    'playback-control.blp',
    'album-tracks.blp',
    'library-view.blp',
    'page-switcher.blp',
    'playlist-view.blp',
    'playlists-page.blp',
    'primary-menu.blp',
    'queue-row.blp',
    'shortcuts-dialog.blp',
    'track-cover.blp',
    'track-details.blp',
    'track-info-dialog.blp',
    'volume-control.blp',
    'window.blp',
  ),
  output: '.',
  command: [
    find_program('blueprint-compiler'),
    'batch-compile',
    '@OUTPUT@',
    '@CURRENT_SOURCE_DIR@',
    '@INPUT@',
  ],
)
