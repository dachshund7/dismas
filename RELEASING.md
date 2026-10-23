Releasing Dismas
================

QA Plan
-------

1. First run
  - Copying Amberol's library, queue, playlists and settings
  - Scanning the Music folder
2. Albums
  - Sorting: artist, title, year, date added
  - Search
  - Open an album; play it; add it to the queue
  - Select albums and tracks, with Shift+click and Ctrl+A
  - Music folders in Preferences: add one, remove one and undo, and
    unplug or rename one, whose albums are hidden until it's back
3. Player
  - Play/pause, with Space too
  - Previous/next
  - Scrubbing
  - Sleep timer, stopping right away and finishing the track
4. Queue
  - Open folder; open file; drag and drop
  - Shuffle
  - Repeat: all, one, continuous
  - Search
  - Drag to reorder
  - Select; remove track; remove all
  - Toggle the playlist pane
5. Playlists
  - Save the queue as a playlist; create, rename, delete
  - Switch between playlists
  - Import and export M3U
  - Restore the queue at startup, resuming paused at the same track and position
6. Desktop
  - Media keys and the media controls of the desktop (MPRIS)
  - Playing in the background

Release
-------

Versions are the year and month of the release, e.g. `2026.10` for October
2026. A second release in the same month adds a number: `2026.10.1`,
`2026.10.2`, and so on. Since the next version depends on when it's
released, set it when releasing, not after.

Checklist for a release.

- [ ] Set the project version in [`meson.build`](./meson.build)
- [ ] Update the [change log](./CHANGES.md)
  - [ ] **Added**: New features, settings, UI, translations
  - [ ] **Changed**: UI updates, improvements, translation updates
  - [ ] **Fixed**: bug fixes, with reference
  - [ ] **Removed**: Removed features, settings, UI; **IMPORTANT**: anything
    inside this list requires a version bump
- [ ] Update the [metainfo](./data/io.github.dachshund7.Dismas.metainfo.xml.in.in)
  - [ ] New `<release>` element
  - [ ] *Optional*: new screenshots
- [ ] `git commit -m 'Release Dismas $VERSION'`
- [ ] `git tag -s $VERSION` (use the change log entry)
- [ ] `git push origin HEAD && git push origin $VERSION`
