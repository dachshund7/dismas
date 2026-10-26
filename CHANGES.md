# Changes

All notable changes to this project will be documented in this file.

## [unreleased]

### Added

- Gapless playback: tracks play back to back, such as movements that run
  into each other
- CUE sheets split audio files into their tracks, from a `.cue` file next
  to them or a `CUESHEET` tag, in the queue, the album grid and playlists,
  each track with its own ReplayGain; `.cue` files can be opened and
  dropped too
- Each playlist resumes where it was, paused at the same track and
  position, at startup and when switching back to it
- A tray icon while playing in the background, to show or hide the window,
  control playback, change the volume by scrolling, and quit
- Switch playlists from a menu on the playlist pane's title
- Drop files between the tracks of the playlist pane, and drag albums from
  the grid into place there
- A drop zone that plays the dropped tracks in place of the queue, with an
  Undo
- The toast after saving the queue as a playlist has an Open button, to
  switch to the new playlist without interrupting playback
- A Preferences dialog (Ctrl+,), with a switch for moving the window from
  its header bars, for window managers that move windows themselves
- Show Waveform, in Preferences, to show a plain seek bar instead of the
  waveform
- Zen Mode, in Preferences: with the playlist hidden, the controls fade
  out once the pointer leaves the window, leaving the cover and the track
  details in the middle, the cover growing into the room the controls
  leave; the pointer or a tap brings them back
- Fullscreen with F11: the player by itself, with larger text and
  controls, in white over a blurred copy of the cover; the controls and the
  pointer fade out while the pointer stays still, as the cover grows into
  their room, and Esc or F11 leaves it
- Change the volume with Ctrl+Up and Ctrl+Down
- Music folders, in Preferences, to show albums from other folders than
  the Music folder, like a second drive; while a folder isn't there, its
  albums are hidden, and they come back without being read again. A music
  folder's row expands to leave out folders in it, such as audiobooks or
  podcasts, whose albums then aren't in the album grid
- Remember the volume, the repeat mode, and whether the playlist pane is
  open
- The track menu has Open Directory, and in the playlist pane Remove,
  which removes all the selected tracks when the track is one of them;
  Go to Album is in the playlist pane, and in an album's tracks while a
  search leaves some of them out
- While a search of the album grid matches only some of an album's
  tracks, its cover says how many, and its track list has a banner with
  Show All, to see and play the whole album. Whole Albums, next to the
  search, makes adding, playing, dragging and opening the albums the
  search found take all their tracks, rather than only those that match
- Recently Played, Most Played and Not Heard Lately, on the Playlists
  page, play the last 100 tracks heard, in the order they were heard, the
  ones played the most in the last 6 months, or the ones played the most
  that weren't heard in the last 2 months, in place of the queue, with an
  Undo
- Mark tracks as favorites with the heart next to the Sleep Timer, Ctrl+D
  or the track menu; Favorites, on the Playlists page, plays them in the
  order they were marked, in place of the queue, with an Undo
- `dismas --add FILE…` adds files and folders to the end of the selected
  playlist, also in a Dismas that's already running, without bringing up
  its window; `dismas --play FILE…` plays them in place of the queue, with
  an Undo
- A Flatpak of each release, `dismas.flatpak`, on the release's page on
  GitHub
- The tags of the Work Hierarchy plugin for Picard are read from MP3, M4A,
  AIFF, WAV, Monkey's Audio, WavPack and Musepack files too, not only from
  FLAC, Ogg and Opus files
- Play only the tracks that a search of the playlist pane found, with the
  button next to the search, in place of the queue, with an Undo; Next and
  the end of a track still go through all the tracks otherwise
- Show in Playlist, in the menu of a track that a search of the playlist
  pane found, closes the search and shows the track among all the tracks
  of the playlist

### Changed

- The album grid takes much less processor time and memory: it only reads
  the covers of the albums on screen, where it read those of hundreds of
  albums around them, again and again while scrolling. Its covers are
  saved as JPEG, which is quicker to read than PNG and takes a fraction of
  the room, and on screens without HiDPI scaling they're made as large as
  the grid shows them, rather than twice as large. They're made again
  once, after updating. The grid also stays as it is after looking for
  changes in the music folders finds none, as at most starts, rather than
  read all of its covers again
- Show the Playing Track and Go to Album glide to the track, rather than
  jumping to it, and the rows of moved tracks slide into place more
  gently, starting slowly instead of at full speed
- Looking for new and changed files in the music folders, at each start,
  is much faster: listing a library of 23,000 files took 3.4 s, and now
  takes 0.2 s
- The waveforms kept in the cache take a tenth of the room, and the
  waveforms and covers in the cache that haven't been used for 90 days are
  removed
- The time played and the time left of a track of an hour or more show
  the hours, as 1:15:00 rather than 75:00
- Background Playback is off by default, so closing the window quits
  unless it's turned on in Preferences; this also applies to anyone who
  never changed the setting
- The settings moved from the main menu to Preferences
- Hidden files and folders in the music folders, like the trash of a
  drive, aren't part of the album grid
- "Restore Playlist at Startup" is now "Restore Queue at Startup": a
  selected saved playlist always reopens, even with the setting off
- Shuffling reorders the playlist for real, so the order shown, played and
  saved is the same, until the shuffle button puts back the order from
  before; the button is filled while the playlist is shuffled
- Shuffling goes by what was heard: tracks heard in the last few days tend
  to go toward the end, and tracks played often that haven't been heard
  for a while tend to come early. The movements of a work stay together and
  in order, and the same artist doesn't come twice in a row unless one
  artist has more than half of the tracks
- Previous and next track are Alt+Left and Alt+Right, instead of Ctrl+B
  and Ctrl+N
- "Match Cover Art" keeps the contrast of the text, with tints of the
  cover's colors that follow the color scheme, takes the accent color from
  the cover, and is off in high contrast
- Once the queue has played to its end, the last track stays shown, and
  playing again starts over from the first one
- Closing the window while playing in the background hides it, so it comes
  back as it was
- Find cover files named cover or folder in any case and image type, such
  as `Cover.JPG` or `folder.webp`, and use them when the cover in the tags
  can't be decoded
- The playlist pane's labels and tooltips name the selected playlist
- Clicking the clear icon of a search entry puts the cursor in it
- Read dates written with dots, such as 2024.06.03
- Tracks without a title tag show their file name, without a leading track
  number, instead of "Unknown title", and can be searched by it; Track Info
  only shows the title and album from the tags
- Tracks without an album tag are in the album grid, grouped by the folder
  they're in and named after it, which is also shown as their album and
  can be searched; a file without tags shows its folder's cover file
- Copy (Ctrl+C) puts only the playing track's title and artist on the
  clipboard, such as "Summer of '69 — Bryan Adams", instead of a
  "Currently playing" sentence in the language of the interface; classical
  tracks have their composers and then their performers, such as "Ein
  deutsches Requiem — Johannes Brahms; Kammerchor Stuttgart & Frieder
  Bernius"
- Track Information shows the artist tag, even when it repeats the
  composer or the performers
- When a classical track's artist is its composer and its tags don't name
  the performers, the player and the queue show the performers from the
  album artist, such as "Kammerchor Stuttgart & Frieder Bernius" from
  "Johannes Brahms; Kammerchor Stuttgart & Frieder Bernius"
- When a track's tags credit an orchestra, a choir, a string quartet or
  another group, the player and the queue name the group and its soloists
  rather than every player, such as "Adrian Chandler · La Serenissima":
  the soloists are those marked solo or named in the album artist, and up
  to four singers. A track that names more than five players and no group
  shows the performers in the album artist instead
- With the tags of the Work Hierarchy plugin, a part of a collection that
  has its own catalogue number, such as each Prelude and Fugue of The
  Well-Tempered Clavier (BWV 846, BWV 847, …), is a work of its own, with
  its own header in the queue and the album view, and a track with a whole
  Prelude and Fugue in it is a track of its own rather than a movement of
  the collection; Track Information shows the work's full title, with the
  collection. The plugin picks the work now, so the files need saving again
  with its version 0.3.0; until then, files saved with an older one show
  their work and movement tags
- A track with only part of a movement, such as half of an overture in two
  tracks, is named after its recording's title, such as "Ia. Overture –
  Adagio", rather than after the whole movement, which the tracks shared;
  a number of an opera keeps its act and its number, as in "Acte I: 8. Air
  « Prendre le dessin d’un bijou » (Gerald)"
- While a search of the playlist pane hides some of the tracks, the tracks
  can't be dragged to another place, and tracks dropped on the pane go to
  the end of the playlist, as two tracks next to each other in the search
  results can be far apart in the playlist. Adding tracks or switching
  playlists closes the search, so the tracks that came in are shown
- A search of the playlist pane that finds nothing says so, rather than
  leaving the pane empty
- Update dependencies

### Fixed

- A long album or artist name in the player scrolled through again at each
  track of the same album, keeping the processor busy for several seconds
  each time
- For a file without its format's main tag, such as an MP3 file with ID3v1
  and APE tags but no ID3v2 tag, the album grid and its covers only read
  the first tag, where the queue filled in from the others
- Movements numbered as parts of another, such as "IIIa." or "12a", got a
  second number in front of theirs, as in "1. IIIa. [Allegro]"
- With "Restore Queue at Startup" off, opening the window again while
  playing in the background replaced the queue with the playing tracks
- Without a Background portal, as in many window manager sessions,
  closing the window quit Dismas even with Background Playback on
- Ctrl+Left and Ctrl+Right seeked instead of moving the cursor in a search
  entry
- Next from the media keys on the last track went back to the first, and
  the desktop's media controls offered Next there
- The desktop's media controls kept the position of the previous track
  after skipping while paused
- When a track repeated, the cover and the waveform were loaded again, the
  queue scrolled back to the track, and a track that decodes slower than it
  plays never got a waveform
- The previous track's waveform stayed until the new one's was ready
- Seeking in a track that hadn't played yet didn't show
- Scrolling to the playing track could leave it out of view, hide the
  tracks before it, or leave out the header of its work
- Remove All and Select stayed on in an empty playlist pane
- Selected albums were queued in the order of the library rather than the
  order of the grid
- A playlist name too long for a file name got the wrong message, and long
  names made tooltips as wide as the screen
- An open track information dialog kept the colors of the cover from when
  it opened
- On X11, Esc didn't cancel dragging a track in the queue, and the queue
  kept scrolling after a track was dropped near its edge
- An album whose tracks had no artist tags was by "Various Artists"
- On X11 without a compositor, drags over the part of the window gained by
  widening it went to the window below
- GLib warnings when nothing was left to play
- GTK themes installed for libadwaita apps, like Colloid, drew the outlines
  and corners of their cards on the covers of the track rows
- The window's size wasn't saved when quitting with Ctrl+Q, only when
  closing the window
- The tops of the capitals in the player's line of performers were cut
  off on some screens
- The playlist pane's title moved off-center in a narrow pane, when the
  time left under it was long
- A work's movements were split under several headers when some of them
  credited other composers too, such as the hymn's author in the chorale
  of a Bach cantata, or Süßmayr in Mozart's Requiem
- Two recordings of a work one after the other, or a work added twice,
  were shown under one header, and shuffled as one
- A program asking over MPRIS for a seek longer than a few centuries, as
  `playerctl position 99999999999+` does, left Dismas unable to go on to
  the next track or take playback commands until it was restarted
- A file whose header gave an impossible length, such as an Ogg file cut
  off in the middle of a page, crashed Dismas when it became the playing
  track or when it was seeked, and at every start if it was the track to
  resume
- A file that the tag reader couldn't cope with stopped every scan of the
  music folders at the same place, so the album grid never got the files
  after it, never forgot removed ones, and never updated its covers
- A playlist file with a NUL byte in a line, such as one saved in UTF-16,
  crashed Dismas when it was opened, and at every start if it was the
  selected playlist
- A damaged library database left the album grid empty for good; it's
  made again
- A playlist could be given a name a few characters too long to save:
  creating it failed, and renaming one to it worked but lost every change
  made to it afterwards
- Amberol's settings were copied again over those of Dismas whenever its
  data folder was missing, such as after clearing it
- A track that couldn't be played, such as one whose file was gone or with
  no decoder for its format, left the player showing it as playing, with
  nothing heard; Dismas says so and goes on to the next track, and greys
  out a track whose file is gone, as when its playlist opens without it
- The tracks of a playlist that weren't found when it opened, such as
  those on a drive that wasn't plugged in, or that couldn't be read, were
  dropped from it for good at the next change to it; they stay in their
  places, greyed out, and playback skips them until their files are back
- Removing many selected tracks at once froze the window for a while,
  and removing the playing track with the ones after it loaded each of
  them into the player in turn
- The parts of an opera's acts that the score doesn't number, such as
  dialogues, recitatives and entr'actes, were numbered by their place in
  the act, which read like the score's numbers, as in "25. Entr’acte"
  right after "No. 11 Finale"; they're named without a number now
- The time left under the playlist pane's title counted all of the
  playing track, however much of it had played, and didn't change when
  tracks were moved or shuffled

### Removed

## [2026.10] - 2026-10-20

The first release of Dismas, a fork of Amberol's development version after
2026.1.

### Added

- An album grid of the Music folder, with search, sorting by artist, title,
  year or date added, and a track list for each album
- Playlists: save the queue as a playlist, create, rename and delete
  playlists, and switch between them on the Playlists page
- Import and export playlists as M3U files, with paths relative to the
  playlist's folder
- Reopen the selected playlist at startup, unless "Restore Playlist at
  Startup" is off
- Search every tag of the tracks in the queue
- Select tracks and albums with Ctrl+click, Shift+click for a range, or
  Ctrl+A, to add, play or remove them together
- Reorder the queue by dragging tracks by their handle, several at once in
  selection mode
- Add the same track to the queue more than once
- Work, movement, composer, performer and production credits for classical
  music, including the tags of the Work Hierarchy plugin for Picard
- Show the name of each work above its movements in the queue, with a line
  between works
- Read the credits in the descriptions of YouTube downloads, which yt-dlp
  stores as `SYNOPSIS`
- A sleep timer, which can let the last track finish
- A track menu, and a track information dialog (Ctrl+I) with every tag,
  links to MusicBrainz, and the format and ReplayGain of the file
- A button that shows the playing track in the queue, and a menu for the
  playing track with Copy, Go to Album and Open Directory
- A Remove All button in the playlist pane
- Resize the playlist pane by dragging its edge; a double click goes back
  to the automatic width
- Shortcuts for the pages: Ctrl+1, Ctrl+2 or Ctrl+G, and Ctrl+3; Page Up,
  Page Down, Home and End scroll the list under the pointer
- Show when the Music folder is being scanned, and how many tracks changed
  after a rescan
- A `move-from-header-bars` setting, with no switch in the UI, to keep the
  header bars from moving the window
- Copy the library, queue, playlists and settings of Amberol on the first run
- Add Norwegian Nynorsk, Telugu, Vietnamese, Thai and Chinese (Taiwan)
  translations

### Changed

- Renamed to Dismas, with the application ID `io.github.dachshund7.Dismas`
- A new icon: Charles Bridge and Prague Castle
- The album grid replaces the start screen
- The window gets as narrow as 360 pixels on every page: the buttons of the
  pages fold into a menu, the titles of the pages are left out, and the album
  grid shows a single column
- Call the list of tracks to play the queue, and audio files "tracks"
- Fit the player to the window, growing the cover and waveform into the
  room left
- Show the cover in the player at full size, rather than scaled up from a
  small copy
- Grow the album covers with the width of the grid
- In a narrow window, push the player aside with the playlist pane rather
  than covering it
- Put the playlist pane toggle and the main menu on every page, and the
  repeat and shuffle buttons beside previous and next
- Move Copy, Open Directory, Track Information and Clear from the main
  menu to buttons and menus next to what they act on
- Toggle playback with Space, rather than Ctrl+P
- Search the queue for every word, ignoring case and accents, rather than
  by fuzzy matching, and keep the order of the queue
- Pause the playing track when it's clicked in the queue, where it shows a
  pause icon while paused
- Go back to the queue search with Ctrl+F rather than closing it, and hide
  the playlist pane with Esc in a wide window too
- Scroll a long label in the player once, rather than over and over
- Load tracks in parallel worker threads, and decode cover art in sandboxed
  glycin loaders
- Scale each channel of the waveform by its own loudest peak
- Update dependencies, including gtk-rs 0.11
- Translation updates

### Fixed

- Don't crash on paths that aren't valid UTF-8, files without a local path,
  or tags that make the reader panic
- Don't write cover thumbnails outside the cache directory for a crafted
  MusicBrainz release ID
- Don't crash at startup on a malformed saved queue, and keep the names
  that aren't valid UTF-8 in it
- Keep the shuffled order in step with changes to the queue
- Keep the current track when removing another one from the queue, and
  keep playing when one track is left
- Move on from the playing track when removing it with Repeat One
- Make the Play button of the "Added a new track" toast play that track
- Keep the MPRIS cover and the album thumbnails up to date
- Support setting the position over MPRIS
- Start playing from the play/pause media key when stopped, and show the
  track in the desktop's media controls at startup
- Stop the previous track's waveform from being drawn over the next one's,
  and don't cache a waveform that couldn't be read whole
- Show "Unknown artist" for tracks without an artist tag
- Find the cover of a track whose extension doesn't match its type, such
  as Opus in an `.ogg` file
- Follow symbolic links to tracks and folders that are opened or dropped
- Keep the Background Playback menu item in step with its setting when the
  portal turns it down
- Show the About dialog without a window, while playing in the background
- Name the PulseAudio stream after the application
- Close the main menu with F10
- Keep the playlist pane hidden after narrowing and widening the window
- Scroll to the playing track at its place in the searched queue
- Scroll long lists without stalling, especially on X11

### Removed

- The Add Song and Add Folder actions, and their shortcuts, Ctrl+S and
  Ctrl+A: tracks come from the album grid, or are opened from the file
  manager or dropped on the window

## [2026.1] - 2026-04-05

### Changed

- Update dependencies [!171, !184, !187]
- Switch to Blueprint for UI definition files
- Use AdwShortcutsDialog instead of GtkShortcutsWindow

### Fixed

- Allow opening the containing folder of the current song [#156]
- Handle errors when parsing a cached waveform [#416]
- Do not crash on folder enumeration [#417]
- Show playlist when queuing songs [#232]
- Show playlist when triggering a search [#424]
- Add Ctrl+F to the keyboard shortcuts list [#423]
- Fix wording of the repeat button tooltip [#326]

## [2025.1] - 2025-03-30

### Added

- Add Malayalam translation
- Add Slovak translation

### Changed

- Update dependencies [!176]
- Documentation fixes [!174]
- Require GTK 4.16 [!173]
- Translation updates

### Fixed

- Clear the cover cache alongside the queue

## [2024.2] - 2024-10-25

### Added

- Use folder.* filenames for cover art [#395]
- Use `XDG_MUSIC_DIR` as the initial folder when adding songs [#373]

### Changed

- Replaced unmaintained mpris-player crate with mpris-server [!150]
- Replaced deprecated gsteamer-player with gstreamer-play [!152]
- Translation updates

### Fixed

- UI and CSS cleanups and improvements [!167]
- Rename the appdata file [#279]

## [2024.1] - 2024-09-21

### Added

- Add a marquee label for song details [!143]
- Add more accessibility labels and descriptions
- Add Romanian translation
- Add Burmese translation
- Add Norwegian Bokmål translation
- Add Hindi translation

### Changed

- Adjust whitespace for DnD overlay [!153]
- Port to the new libadwaita widgets [!138]
- Update dependencies [!144, !155, !160]
- Translation updates

### Fixed

- Support higher scaling factors in the cover image [#344]
- Update application metadata [!145, !157, !164]

### Removed

## [0.10.3] - 2023-05-23

### Changed

- Translation updates
  - Hebrew
  - Polish
  - Finnish
  - Portuguese
  - Russian
  - German

### Fixed

- Resize the waveform view when interpolating states
- Fix cover art scaling [#324]
- Fix deprecation warnings at build time

## [0.10.2] - 2023-04-28

### Changed

- Use a better icon for the drop overlay [!135]
- Translation updates
  - Galician
  - Slovenian
  - Hungarian
  - Swedish
  - Turkish
  - Indonesian
  - French
  - Occitan

### Fixed

- Crash when adjusting volume while in background [#317]
- Crash when adding folder after sending app to the background [#319]

## [0.10.1] - 2023-04-25

### Fixed

- Background playback doesn't work [#314]

## [0.10.0] - 2023-04-24

### Added

- Allow restoring the playlist from the previous session [!120]
- Allow disabling background playback
- Add control to quickly muting/unmuting the audio
- Enable ashpd on FreeBSD [!130]
- New translations
  - Belarusian
  - French
  - Korean
  - Friulan
  - Czech

### Changed

- Update the dependency to the gtk-rs bindings
- Update the dependency to ashpd
- Update the dependency to lofty
- Use consistent labelling for adding single songs [#270]
- Use consistent terms for songs and playlist [#301]
- Improve rendering of cover art [#310]
- Separate the volume bar from the waveform display
- Translation updates
  - Ukrainian
  - German
  - Slovenian
  - Portuguese
  - Brazilian Portuguese
  - Georgian
  - Russian
  - Croatian
  - Danish
  - Turkish
  - Swedish
  - Hebrew
  - Polish
  - Indonesian
  - Occitan
  - Lithuanian
  - Occitan
  - Galician
  - Finnish
  - Basque
  - Korean
  - Persian
  - Spanish
  - Hungarian
  - Serbian

### Fixed

- Reset the playlist position when searching [!117]
- Use HIG-compliant capitalization in the shortcuts view [!115]
- Narrow playlist view [#230]
- Do not add duplicate songs to the playlist [#248]
- Update next track button state depending on repeat mode [!114]
- Fix styling of the search bar [#266]
- Stop waveform pipeline when dropping the generator [!107]
- Change the ReplayGain menu translatable context [#281]
- Always draw last bar in waveform view [!127]
- Fix waveform overdrawing in RTL layout [!128]
- Fix waveform overdrawing [!124]
- Improve the readability of the drop overlay [#288]

### Removed

## [0.9.2] - 2022-12-10

### Added

- Allow continuous seeking on the waveform widget [#99]
- New translations
  - Icelanding
  - Hungarian
  - British English
  - Greek

### Changed

- Use new about window from libadwaita
- Update build to use Cargo directly without a wrapper
- Update the dependency on lofty to 0.9.0
- Translation updates
  - Finnish
  - Persian
  - Portuguese
  - Turkish
  - Chinese (China)
  - Italian
  - Swedish
  - Brazilian Portuguese
  - Danish
  - Occitan
  - Slovenian
  - Hebrew
  - Indonesian
  - German
  - Russian
  - Serbian
  - Basque
  - Georgian
  - Dutch

### Fixed

- Recolor the folded playlist background
- Check for more file names for external covers [#247]
- Force the direction of the primary menu
- Remove toast for failed background portal requests
- Fully animate waveform between tracks [#254]

## [0.9.1] - 2022-08-30

### Added

- Add ReplayGain support [#75]
- Show hours to the playlist time
- Support external cover art files [#14]
- Add playing indicator in selection mode [#227]
- New translations
  - Serbian

### Changed

- Stabilise the shuffling behaviour [!104, #207]
- Update the version of lofty [#216]
- Translation updates
  - Russian
  - Portuguese
  - Persian
  - Polish
  - Ukrainian
  - Basque
  - Turkish
  - Italian
  - Finnish
  - Croatian
  - Occitan

### Fixed

- Clarify the notification text for unavailable files [#215]
- Increase specificity of the cover art UUID
- Make the playlist side bar narrower [#230]
- Check for unsigned overflow [#223]
- Properly mark playlist remaining time for translation [#225]

### Removed

- Remove the "sound" keyword from the desktop file [#224]

## [0.9.0] - 2022-08-05

### Added

- Add key shortcut for toggling playlist shuffle [!94]
- Add fuzzy search to the playlist [!96]
- New translations
  - Catalan
  - Lithuanian
  - Georgian
  - Italian

### Changed

- Only list audio MIME types supported by lofty
- Update the version of lofty we depend on
- Translation updates
  - Swedish
  - Portuguese
  - Basque
  - Russian
  - Polish
  - Persian
  - Ukrainian
  - Dutch
  - German
  - Indonesian
  - Chinese (China)
  - Hebrew
  - Brazilian Portuguese
  - Occitan
  - Finnish

### Fixed

- Add the directory MIME type to the list of supported types [#202]
- Explicitly remove action bar background [!95]
- Make sure to maintain aspect ratio of cover art pixbuf [#208]

### Removed

- Remove MIME type associations for unsupported file types

## [0.8.1] - 2022-06-24

### Added

- New translations
  - Danish

### Changed

- Use the appropriate wording and style for tooltips [!89]
- Require version 0.4.8 of the gtk4 crate [!90]
- Translation updates
  - Ukrainian
  - German
  - Hebrew
  - Russian
  - Polish
  - Swedish
  - Occitan
  - Portuguese
  - Chinese (China)

### Fixed

- Allow selecting multiple files and folders [#71]
- Fix improper MPRIS reporting when paused [#201]
- Fix panic on playback state change with no window [!91]
- Maintain the playlist panel's width [#190]
- Use the display name as the song base UUID [#198]
- Handle nested dist folders properly
- Let Amberol run in the background without a window [!83]
- Sort files like Nautilus when adding a folder [#187]

## [0.8.0] - 2022-06-17

### Added

- Implement playlist search [#178]
- Restore the window state [!79]
- Support building and running on macOS [#179]
- Add cover cache object [!74]
- Allow running Amberol in the background [!72]
- New translations
  - Finnish
  - Portuguese
  - Nepali
  
### Changed

- Flip the waveform channels [!76]
- Notify the user when drag and drop gives us no files [#175]
- Update the dependency on lofty [#172]
- Translation updates
  - Polish
  - Ukrainian
  - Swedish
  - Hebrew
  - Brazilian Portuguese
  - German
  - Basque
  - Occitan
  - Persian
  - Russian
  - Chinese (China)

### Fixed

- Reset the player state when removing its last song [#170]
- Disable queue.clear action while adding songs [#163]
- Mark file selection dialog titles for translation [#164]
- Start playing when selecting the current row
- Set min-height for the song details [#155]
- Restore queue actions once loading ends [#160]

### Removed

## [0.7.0] - 2022-06-03

### Added

- Show the current song in the window title
- Add a warning for failed cover art loading
- Show toast when adding a single song [#136]
- New translation
  - Slovenian

### Changed

- Change the currently playing song indicator [#74]
- Improve vertical spacing of playback controls [!70]
- Make the shuffle model more predictable [!67]
- Refine the app icon [!66]
- Rework waveform colors [#119]
- Tweak the cover art style in the playlist [!63, #147]
- Use a single suggested action button [#145]
- Adjust scale and progress bar styles [#146]
- Translation updates
  - German
  - Polish
  - Ukrainian
  - Swedish
  - Russian
  - Turkish
  - Hebrew
  - Dutch

### Fixed

- Do not change position when scrubbing without a song [#151]
- Fix double select on playlist end [#149]
- Notify if no files/folders were selected [#148]
- Add a check for MPRIS cover art
- Fix handling cleared queues [#138]
- Use a weak reference when loading songs [#140]
- Update playlist length when removing a single song
- Switch window mode when opening a file

## [0.6.3] - 2022-05-21

### Added

- Add translations:
  - German

### Changed

- Translation updates:
  - Swedish

### Fixed

- Fix drag and drop on the initial landing page [!62]

## [0.6.2] - 2022-05-20

### Added

- Add "copy song details to clipboard" [!53]
- Add a cache for the waveforms, to speed up loading on songs we have
  already seen [#131]
- Add accessibility information to various custom widgets
- Add translations:
  - Chinese (China)

### Changed

- Add whole folder at once [!30]
- Provide user feedback during loading [!54]
- Translation updates:
  - Swedish
  - Ukrainian
  - Occitan
  - Basque
  - Turkish
  - Polish
  - Russian
  - Persian

### Fixed

- Ensure that the remaining time sign is consistent in RTL locales [#118]
- Improve the UI consistency when clearing the playlist
- Apply darkening to the playlist view unconditionally [#128]
- Fix the playlist end state [#132]
- Fix key navigation [#130]

## [0.6.1] - 2022-05-09

### Added

- Add translations for:
  - Dutch
  - Indonesian
  - Occitan
  - Spanish

### Changed

- Make the cover art image slightly bigger
- Update translations for:
  - Ukrainian
  - Swedish
  - Polish
  - Persian

### Fixed

- Improve the appearance of the initial landing page [#106]
- Stabilise the width of the playlist panel [#110]
- Rely on gdk-pixbuf instead of lofty for image format detection [#111]
- Multiple papercut style fixes [#105, #108]
- Fix selector for playlist background when folded [#107]

## [0.6.0] - 2022-05-06

### Added

- Use a selection mode for the playlist management [#81]
- Allow disabling UI recoloring
- Expose more song state through MPRIS
- Add better error messages in the UI
- Support RTL text direction in the waveform widget
- Add translations for:
  - Russian
  - Turkish
  - Brazilian Portuguese
  - Hebrew
  - Galician
  - Swedish
  - Basque
  - Persian
  - Ukrainian
  - Polish

### Changed

- Reset to the initial state when clearing the playlist [#101]
- Tone down the recoloring to improve legibility of text and controls [#97]
- Recolor only the main window [#104]
- Use better icon for playlist toggle button [#102]
- Use the cover art palette for the waveform view accent color [#61]
- Set the minimum and maximum width for the playlist view [#93]

### Fixed

- Fix the background recoloring gradient to use the whole cover art palette
- Reset the waveform generator and view on failure [#57]
- Darken the playlist background when unfolded [#85]
- Improve the tooltips for playback controls [#69]
- Fix extra spacing in the playlist view [#98]
- Fix elapsed song time in RTL locales [#95]
- Remove missing shortcuts from the shortcuts dialog [#96]

## [0.5.0] - 2022-04-29

### Added

- Improve fallback paths for song metadata

### Changed

- Move the playlist side panel to the left of the playback controls [#50]
- Make sure that the remove button in the playlist rows is accessible
  without hovering

### Fixed

- Align the waveform to the pixel grid [#76]

### Removed

- Drop the seek buttons, and rely on the waveform control [#59]

## [0.4.3] - 2022-04-26

### Added

- Add scrolling support to the volume control [#50]

### Fixed

- Fix behaviour of the waveform with short songs and avoid overdrawing [#68]
- Make the waveform control more legible [#52]
- Reset the shuffle state when clearing the playlist [#60]
- Keep the playlist visibility, folded or unfolded, in sync with the
  toggle button that controls it [#55]
- Fix a crash when manually advancing through the playlist [#54]

## [0.4.2] - 2022-04-22

### Fixed

- Fix the fallback cover art in the playlist

## [0.4.1] - 2022-04-22

### Fixed

- Don't skip songs without a cover art [#46]
- Clean up unnecessary overrides [Bilal Elmoussaoui, !32]

## [0.4.0] - 2022-04-22

### Added

- Add waveform display and quick navigation
- Allow queueing folders recursively
- Add initial status page at startup [#27]
- Add remove button to the playlist [#40]
- Show cover art in the playlist

### Changed

- Allow adding folders via drag and drop [#17]
- Allow shuffling only when the playlist contains more than one song [#15]
- Style the popover using a similar background as the main window [#12]
- Small style tweaks for the recoloring
- Reduce the height of the full window to fit in 768p displays [#16]
- Make the layout more mobile friendly [#28]
- Ship our own icon assets

### Fixed

- Fix an assertion failure when reaching the end of a shuffled playlist
- Scroll playlist to the current song [#29]
- Update dependency on lofty for m4a support [#22]
- Add divider above scrolling playlist [#26]
- Fix styling of the missing cover fallback image [#36]
- Set the album art metadata for MPRIS [#13]

## [0.3.0] - 2022-04-15

### Added

- Allow shuffling the contents of the playlist
- Support dropping multiple files
- Volume control
- Allow Amberol to be set as the default application for Music in
  the GNOME Settings

### Changed

- Miscellaneous cleanups [Christopher Davis, !10]
- Use idiomatic Rust as suggested by Clippy
- Improve handling the end of playlist state
- Skip songs that cannot be queried for metadata
- Switch to a portrait layout

### Fixed

- Stop playback when clearing the playlist
- Immediately play the song selected from the playlist
- Use the appropriate color format for the texture data [#7]
- Use the proper fallback asset for albums with no cover
- Start playing when opening a file [#8]

## [0.2.1] - 2022-04-11

### Changed

- Style tweaks [Jakub Steiner, !9]

### Fixed

- Handle songs with unset fields without panicking

## [0.2.0] - 2022-04-11

### Added

- Inhibit system suspend when playing

### Changed

- Tweak the behaviour of the window when toggling the playlist
- Improve the style of the window [Alexander Mikhaylenko, !7]
  - Deal with margins and padding
  - Style the playlist list view
  - Style the drag overlay

## [0.1.0] - 2022-04-08

Initial alpha release for Amberol

### Added

- Basic playback
- Playlist control:
  - Add single file
  - Add folder
  - Drag and drop
- Support opening files from the CLI
- Recolor the UI using the cover art palette
