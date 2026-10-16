Releasing Amberol
=================

QA Plan
-------

1. Initial state
  - Open folder
  - Open file
  - Drag and drop
2. Main view
  - Play/pause
  - Previous/next
  - Scrubbing
3. Queue
  - Shuffle
  - Repeat: all, one, continuous
  - Select
  - Remove track
  - Remove all
  - Toggle queue

Release
-------

Checklist for a release.

- [ ] Update the [change log](./CHANGES.md)
  - [ ] **Added**: New features, settings, UI, translations
  - [ ] **Changed**: UI updates, improvements, translation updates
  - [ ] **Fixed**: bug fixes, with reference
  - [ ] **Removed**: Removed features, settings, UI; **IMPORTANT**: anything
    inside this list requires a version bump
- [ ] Update the [appdata](./data/io.bassi.Amberol.appdata.xml.in.in)
  - [ ] New `<release>` element
  - [ ] *Optional*: new screenshots
- [ ] `git commit -m 'Release Amberol $VERSION'`
- [ ] `git tag -s $VERSION` (use the change log entry)
- [ ] Bump up the project version in [`meson.build`](./meson.build)
- [ ] `git push origin HEAD && git push origin $VERSION`

Flathub
-------

- [ ] Update the `io.bassi.Amberol.json` manifest
  - [ ] Change the archive URL
  - [ ] Change the SHA256 checksum
- [ ] `git push origin HEAD`
