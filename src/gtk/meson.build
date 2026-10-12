# SPDX-FileCopyrightText: 2025  Emmanuele Bassi
# SPDX-License-Identifier: GPL-3.0-or-later

blueprints = custom_target('blueprints',
  input: files(
    'playback-control.blp',
    'playlist-view.blp',
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
