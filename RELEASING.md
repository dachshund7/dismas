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
- [ ] Run the Release workflow by hand on the branch to release, and try the
  Flatpak and the tarball from the run's artifacts. CI doesn't build them
  with the release's optimizations (LTO), so this is the only build like the
  release's before the tag
- [ ] `git commit -m 'Release Dismas $VERSION'`
- [ ] `git tag -s $VERSION -m "Dismas $VERSION"`
- [ ] `git push origin HEAD && git push origin $VERSION`; the tag makes the
  Release workflow build the Flatpak and the prebuilt tarball, and put them
  in a draft release as `dismas.flatpak` and `dismas-$VERSION-x86_64.tar.xz`,
  with the version's section of the change log as its notes
- [ ] Check the draft's files and notes, wait until the CI workflow has
  passed on the release commit and the last run of the Advisories workflow
  on main has passed too, and publish the draft. The releases are
  immutable (Settings → General → Releases): once published, a release's
  tag and files can't be changed, so a mistake found afterwards needs a new
  version

Dependencies
------------

At the start of each cycle, right after a release, on the branch for the
next one, so that the new versions are used for a month before they ship:

- [ ] `cargo update`, then build and run the tests
- [ ] `cargo update --verbose` lists the crates with newer versions that
  `Cargo.toml` doesn't allow, as `Unchanged … (available: …)`; take the
  ones worth the changes they need. For lofty, `cargo test lofty_gaps`
  says what to change (see CLAUDE.md)
- [ ] Update the actions in `.github/workflows/`, in all the files
  together, to releases at least two weeks old: pinned to the release's
  commit, with the release in the comment
- [ ] When a new GNOME runtime is out (in March and September), move the
  `runtime-version` of both Flatpak manifests and the `gnome-NN` image of
  the CI and Release workflows to it, together

When the Advisories workflow fails, read the advisory:

- If Dismas reaches the code it's about, update the crate on main (`cargo
  update -p <crate>`), release a new version with it, e.g. `2026.11.1`,
  and rebase the branch for the next release on main
- Otherwise, add the advisory's ID to `ignore` in
  [`deny.toml`](./deny.toml), with the reason
