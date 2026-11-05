# Changes

All notable changes to this project will be documented in this file.

## [unreleased]

### Added

- The Performers list has an Artists section, with the album artists of
  albums that aren't classical works, such as bands, even when the tags
  credit the band's members, or the artists of the tracks of a
  compilation, and the performer: search finds them too; Performers so
  shows in a library with no conductors or soloists in its tags too. A
  composer given as the album artist isn't listed, and the library is
  read again once
- A playlist can be looked at without switching to it: on the Playlists
  page, its row shows its tracks in the pane, over the queue, with how
  many there are and how long they last, while what plays goes on. Play
  there switches to it and plays it, from where it was or from the track
  clicked, and the queue stays as it was, to switch back to; Play Next
  and Add put its tracks in the playlist that's playing
- The smart playlists show the tracks they pick in the pane in the same
  way, before playing them, and Play plays those tracks in place of the
  queue, with an Undo
- A Tracks toggle on the Albums page, beside Sort and Select (Ctrl+T),
  takes only the tracks that the search, the composer and the performer
  picked, and the heart match from an album you open, play, add or drag,
  as the Tracks page did; from another page, Ctrl+T shows the albums with
  it on. It's off at each start
- The covers of the albums that a search matches only partly say how
  many of their tracks it does, whether or not the toggle is on, rather
  than only on the Tracks page
- The desktop's media controls and other programs that use MPRIS can read
  and change the volume, and see it change when the volume slider moves
- They can turn Shuffle on and off too, as the Shuffle button does, and
  see when it changes, and they can quit Dismas and make it fullscreen

### Changed

- Switching to a playlist from the Playlists page is Switch To, in its
  row's menu, as a click on the row shows its tracks; the menu of the
  playlist pane's title still switches with one click
- The toasts after adding tracks to another playlist, or saving the
  queue as a playlist, have a Show button, which shows its tracks in the
  pane, rather than Open, which switched to it
- A playlist saved from the queue goes on from where the queue was, once
  it's played
- The sections of the Performers list stand out, with a line above each
  and its name in bold, and screen readers say which section a performer
  is in
- Screen readers name the rows of the Composers and Performers lists,
  with what the performers play or sing and their number of albums
- Go to Work glides to the work, as Show the Playing Track glides to a
  track, rather than jumping to it
- The files that only GStreamer can read, such as Ogg FLAC, Matroska and
  WMA files, are read in a sandbox: the process that reads each one can
  only read the system's files and that file, and can't write files,
  reach the network or start a program, so a crafted file that took over
  a GStreamer plugin can't do anything else either. It only gets the
  settings GStreamer needs from Dismas's environment, not tokens or the
  like. It's made with Landlock and seccomp, which the Flatpak can use
  too
- Ctrl+R changes the repeat mode, as in Spotify and Rhythmbox, next to
  Ctrl+S for shuffling, rather than Ctrl+T; F5 alone scans the music
  folders for changes
- Ctrl+3 shows the works, rather than Ctrl+4, as the Tracks page is gone
- Open Directory, in the menus of tracks and albums, is Open Folder, the
  word the rest of Dismas uses
- Update dependencies, including rusqlite 0.41, which needs SQLite 3.45.3 or
  later

### Fixed

- The covers of a work on the Works page often shook as they slid open
- A work opened while the works were read again, as after a scan or a
  change to the favorites, could stay closed
- Show the Playing Track and Show in Playlist left the queue where it
  was, or made it flicker, while it was still moving on after a fling of
  the touchpad or a quick spin of the mouse wheel
- The arrow keys did nothing on the cover that Go to Work gave the focus
  to
- Play on the tracks of an album, with the Playlists page over the
  player, played them without showing the player
- Play on an album with its tracks still being looked up emptied the
  queue, with nothing to put in it and no Undo
- Once a dialog had closed, Enter on a row of the Playlists page, or of
  any list on a page other than the player, played or paused instead of
  opening the row
- The Menu key and Shift+F10 open the menu of a playlist on the
  Playlists page, as they do on the rows of other lists
- A music file whose ID3v2 tag held chapters nested inside each other
  dozens deep, which only a crafted file does, crashed Dismas, and at
  every start, as the library reads its files then. Such a tag is now
  refused, and GStreamer reads the file instead
- After a program changed the repeat mode over MPRIS, the desktop's media
  controls could go on showing the mode before
- After the queue was cleared, the desktop's media controls went on
  showing the last track, with Play on
- The desktop's media controls offered Pause and seeking with no track,
  and Previous on the first track, where it did nothing
- A program asking over MPRIS for a position past the end of a track sent
  Dismas on to the next track
- Stop from the desktop's media controls left the position where it was,
  though playing again started the track over
- The desktop's media controls were told that a track whose length isn't
  known lasted 0 seconds
- Programs that read the position and length of a track over MPRIS got
  whole seconds, so synced lyrics or a progress bar could be up to a
  second off
- Seeking only moved in whole seconds from the last whole second, so a
  program seeking half a second over MPRIS moved nothing, and the seek
  keys jumped back a moment as well
- When the tags of the playing track were read again, the desktop's media
  controls were told that another track had started
- A music file with a NUL character in its artist, composer, genre or
  album artist tag, which only a crafted file has, cut off the desktop's
  media controls when it became the current track, until Dismas was
  started again

### Removed

- The Tracks page; the Tracks toggle on the Albums page takes its place

## [2026.11] - 2026-11-02

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
  leave; the pointer, a tap or a key brings them back
- Fullscreen, with F11 or from the main menu: the player by itself, with
  larger text and controls, in white over a blurred copy of the cover; the
  controls and the pointer fade out while the pointer stays still, as the
  cover grows into their room, and come back with the pointer or a key;
  Esc or F11 leaves it
- Change the volume with Ctrl+Up and Ctrl+Down
- Music folders, in Preferences, to show albums from other folders than
  the Music folder, like a second drive; while a folder isn't there, its
  albums are hidden, and they come back without being read again. A music
  folder's row expands to leave out folders in it, such as audiobooks or
  podcasts, whose albums then aren't in the album grid
- Remember the volume, the repeat mode, and whether the playlist pane is
  open
- The track menu has Open Directory, and in the playlist pane Remove,
  which removes all the selected tracks when the track is one of them,
  and Go to Album
- Recently Played, Most Played and Not Heard Lately, on the Playlists
  page, play the last 100 tracks heard, in the order they were heard, the
  ones played the most in the last 6 months, or the ones played the most
  that weren't heard in the last 2 months, in place of the queue, with an
  Undo
- Mark tracks as favorites with the heart next to the Sleep Timer, Ctrl+D
  or the track menu; on the Tracks page, the heart beside the search shows
  them, and Select All and Play play them
- `dismas --add FILE…` adds files and folders to the end of the selected
  playlist, also in a Dismas that's already running, without bringing up
  its window; `dismas --play FILE…` plays them in place of the queue, with
  an Undo
- A Flatpak of each release, `dismas.flatpak`, on the release's page on
  GitHub
- A prebuilt tarball of each release for x86_64,
  `dismas-<version>-x86_64.tar.xz`, on the release's page on GitHub, for
  distributions with GNOME 49's libraries or newer: it runs from wherever
  it's unpacked, and its `install.sh` adds it to the menu
- The tags of the Work Hierarchy plugin for Picard are read from MP3, M4A,
  AIFF, WAV, Monkey's Audio, WavPack and Musepack files too, not only from
  FLAC, Ogg and Opus files
- Play only the tracks that a search of the playlist pane found, with the
  button next to the search, in place of the queue, with an Undo; Next and
  the end of a track still go through all the tracks otherwise
- Show in Playlist, in the menu of a track that a search of the playlist
  pane found, closes the search and shows the track among all the tracks
  of the playlist
- The searches, of the playlist and of the albums, tracks and works, take
  fields, as in composer:bach, year:<1970, catalogue:bwv1046..1051,
  released:-30d or length:>10min, with OR, "-" or NOT to leave tracks
  out, parentheses, and quotes for phrases. composer:(bach OR handel)
  looks for either in one field, has:composer finds the tracks with a
  composer, and is:live, is:favorite and other flags find the tracks they
  name. The play history and the hearts can be searched too:
  plays:10.. played:<-2m finds tracks played often but not lately, and
  added:-30d, favorited:2026 and is:favorite-album find what they say.
  Search Syntax, in the main menu, lists them all. A search with a
  mistake, like year:abc, finds nothing, and a line under it says why,
  which screen readers announce
- Searches find curly quotes and long dashes when they're typed as plain
  ones, so "bach's" finds "Bach’s" and "kyrie - christe" finds "Kyrie –
  Christe", and they ignore how many spaces are between words
- Searches find a catalogue number typed without its space, so "bwv1047"
  finds "BWV 1047", in the work or in the title
- The album covers can be made larger or smaller, on the Albums, Tracks
  and Works pages, with the buttons at the top of the main menu, with
  Ctrl+plus and Ctrl+minus, with Ctrl and the mouse wheel over them, or
  with a pinch on a touchscreen or a touchpad; Ctrl+0 puts them back to
  their normal size. Smaller, more of them fit on a row. When they're
  made larger, the covers are made again at that size, which the
  progress bar counts, and they're made smaller again once they're back
  to their normal size
- The text of the lyrics can be made larger or smaller the same way: with
  Text Size, in their header, with Ctrl+plus, Ctrl+minus and Ctrl+0 while
  the focus is on them, or while they show beside the player, and with
  Ctrl and the mouse wheel or a pinch over them
- Opening an M3U playlist with Dismas, from a file manager or on the
  command line, opens its tracks, as it does a folder; it said it found
  none. They're found as when the playlist is imported
- WebM files are read as tracks, and their sound plays; a video in them
  isn't shown
- Play Next, in the menu of a track, puts it after the playing track, or
  after the last movement of the work that's playing, so that the work
  isn't cut; tracks played next one after the other play in the order they
  were chosen. In the playlist pane, it moves the track rather than adding
  it again
- Add to Playlist, in the menu of a track, adds it to another playlist
  without switching to it, so the music goes on, or to a new playlist; the
  toast has an Open button to switch to it
- The albums in the grid have a menu, with a right click or a long press:
  Play, Play Next, Add to the selected playlist and Add to Playlist, which
  take all the selected albums on an album selected in selection mode, and
  Open Directory, which opens the album's folder
- The Add buttons of an album's tracks and of the albums selected in the
  grid have a menu next to them, with Play Next and the other playlists to
  add to
- Stop After This Track and Stop After This Work, in the popover of the
  sleep timer, stop the music at the end of the playing track, or at the
  end of the work it's a movement of, letting its movements play through
  without a gap
- New, changed and removed tracks in the music folders show in the album
  grid by themselves, a couple of seconds after their files stop changing,
  and so do the albums on a drive once it's plugged in. Folders on network
  shares, and in the Flatpak those added through the document portal, are
  read again when the album grid is shown instead, at most every 5 minutes
- Keep a Play History, in Preferences, to stop adding the tracks played
  to the history, and Clear History, to delete it; the favorites are kept
- Play a Random Album, Play a Random Track or Play a Random Work, in the
  menu of the Albums, Tracks and Works pages, plays an album, a track or a
  work picked at random from those the page shows, in place of the queue,
  with an Undo. The search, the composer and performer picked, and the
  heart beside the search narrow what it picks from. An album, a track or
  a work not heard lately is more likely to be picked, and choosing it
  again plays another. A work plays in one of its recordings that the
  search matches; with the heart on, in a favorite recording if it has one.
  A long work is more likely to be picked than a short one, and a whole
  recording than an excerpt of the same work. When the recording is only a
  movement of the work, such as an aria of an opera, or an arrangement of
  it, the toast says so
- Works and recordings can be favorites, not only tracks: Add Work to
  Favorites is in the menu of a work's header and of the rows of the
  Works page, with a right click, a long press or the Menu key, and Add
  Recording to Favorites in the menu of the cover of a recording. The
  Works page shows a heart on the rows of favorite works, and on the
  covers of the favorite recordings of an open work, and the heart beside
  its search shows only them
- Albums can be favorites too: Add Album to Favorites is in the menu of an
  album of the grid, with a right click, a long press or the Menu key, and
  takes all the selected albums in selection mode, and the heart in the
  header of an album's tracks marks the album. Favorite albums have a heart
  on their cover. An album stays a favorite when a change to its tags, such
  as a new title or a MusicBrainz id, makes it another album to the
  library, as long as the file of its first track stays where it was. On
  the Albums page, the heart beside the search shows only the favorite
  albums. On the Tracks page, it shows the albums with favorite tracks,
  and takes only those tracks from them: an album opened there lists only
  its favorite tracks, under a bar that says how many of its tracks they
  are. The page says how to mark favorites when there are none
- Composers, the button next to the album search, lists the composers of
  the albums, sorted by their last names, with the number of their albums;
  the names are shown last name first only where the tags give them so,
  as a band can be a composer too. Picking one shows only their albums,
  and on the Tracks page takes only the tracks that are theirs, as a
  search does; it combines with the search
- Performers, the button next to Composers, lists the conductors, the
  ensembles and the soloists of the albums, with what they play or sing
  and the number of their albums; picking one shows only their albums,
  and on the Tracks page takes only the tracks they're on; it combines
  with a composer and with the search
- With a composer picked, or a search, Performers lists only the
  performers of the tracks they match, with the number of those albums,
  and Composers does the same with a performer picked. On the Works page,
  they list only those of works, with the number of those works, so that
  picking one always shows some
- A Tracks page, next to the albums (Ctrl+3), shows the same albums, with
  the same search and the same composer and performer picked, but takes
  only the tracks they match, rather than whole albums: the cover of each
  album says how many, and an album opened there lists only those tracks
- A Works page, next to the tracks (Ctrl+4), lists the works of the
  albums under the names of their composers, with the number of their
  recordings. A composer's works are in the order of their catalogue
  numbers, BWV 1004 before BWV 1007: by the catalogue that most of them
  are in, then by any other, as Beethoven's WoO after his opus numbers,
  and then the works without a number, by title. It shares the albums'
  search and the composer and performer picked, so switching between the
  pages keeps them. Clicking a work, or Enter, opens it, one at a time, to
  show a cover for each of its recordings, the oldest first, each with its
  performers and the year it was made; an album with two performances of
  a work, or with the work and an arrangement of it, has a cover for each.
  A cover takes only its recording's tracks from its album, as the Tracks
  page takes the tracks a search matches: its badge says how many, and it
  opens the album with only those tracks. Several covers can be selected,
  as on the Albums page, to play or add their recordings at once, and Go
  to Album, in the menu of a cover, shows its album on the Albums page and
  opens it there
- Go to Work, in the menu of a track of a work and in the menu of the
  header of a work, with a right click or a long press, opens the work on
  the Works page, with the cover of the track's recording pointed out
  among those of the work's other recordings
- Searching the queue and the albums also finds tracks by the names that
  the Work Hierarchy plugin gives their works, the works those are part
  of and the collections they're in, such as "Well-Tempered" for the
  Preludes and Fugues, and by the other names their works have on
  MusicBrainz, in any language, such as "Trout" for Schubert's
  "Forellenquintett". The other names need version 0.6 of the plugin
- Track Information says when a track was recorded, from the
  RECORDINGDATE tag that the Work Hierarchy plugin writes from MusicBrainz
- The tracks of an album show when it was made under its title, with its
  record label and catalogue number
- The player shows the year after the album, as in "Cello Suites · 1961"
- Open Booklet opens the booklet of an album in the app for PDF files:
  from the menu of an album and of a track, the More menu of the player,
  and a button over the tracks of an album. Any PDF in the album's
  folder counts, whatever its name, and for an album whose discs are in
  folders of their own, one in the folder above them too; with several,
  the item lists them by name
- The tracks of an album on several discs show where each disc starts,
  with its number and its subtitle, such as "Disc 2 · Act II", or the name
  of its folder for discs in folders of their own. A work that goes on to
  the next disc is named again under it
- Automatic, in the ReplayGain settings, takes the album gain while the
  tracks of an album play in their order, so that the quiet movements of
  a symphony stay quiet, and the track gain otherwise, as in a shuffled
  queue or a playlist of tracks from many albums
- Pre-amp, in the ReplayGain settings, plays every track louder or
  quieter than its ReplayGain, by up to 15 dB, those without ReplayGain
  tags too, but never raises a track past its peak
- ReplayGain also takes the R128 gains of Opus files (R128_TRACK_GAIN and
  R128_ALBUM_GAIN), which some tools write in place of ReplayGain tags, as
  the Opus format has them; those files played without their gains, and
  Track Information shows them now
- A work that MusicBrainz has as an arrangement of another, which often
  has the same title, says so in its header, with what it is when
  MusicBrainz says, such as "for two pianos, Busoni", and Track
  Information names the work it arranges. So does a track that
  MusicBrainz links to an arrangement of a single movement, such as a
  song of a cycle arranged for a piano trio. On the Works page, an
  arrangement is listed under the work it arranges when that work is in
  the music folders and they share a composer: the work opens to show
  the covers of its arrangements after its own, each saying what it is,
  and Go to Work on a track of an arrangement goes there. Otherwise the
  arrangement is listed on its own, with what it is after its name. This
  needs version 0.5 of the Work Hierarchy plugin
- The Menu key and Shift+F10 open the menu of the track or the album
  that has the focus, as a right click does
- Alt+Up and Alt+Down move the track with the focus in the playlist pane
  a place up or down, along with the selected tracks if it's one of them
- Ctrl+Z undoes replacing the queue, while the toast that says so is up
- The waveform shows when it has the focus. Holding an arrow key seeks
  on, and Home and End go to the start and the end of the track, as in a
  slider, which keeps Home, End, Page Up and Page Down too
- Show the Playing Track, Show in Playlist and Go to Album put the focus
  on the track they show, and Go to Work on the cover of its album, so
  that the keys go on from there
- Add Tracks… and Add Folder…, in the main menu (Ctrl+O and Ctrl+Shift+O),
  add tracks from outside the music folders to the selected playlist, as a
  phone has nothing to drag them from. They're back from before 2026.10,
  under new names and shortcuts
- On a touchscreen, a swipe to the right on the tracks of an album or a
  work goes back to the playlist
- On a touchscreen, the handle to drag a track in the playlist pane is
  easier to grab, and over a narrow window, a swipe that starts on the edge
  of the pane hides it, rather than resizing it
- Ctrl+T changes the repeat mode, and Ctrl+M mutes and unmutes
- Ctrl+W closes the window, as in other GNOME apps, or the dialog that's
  open over it. While playing in the background, the window hides and the
  music goes on, as with its close button
- Copy, in the menu of a track, copies its details. Ctrl+C does the same
  for the track that has the focus in the playlist pane or an album's
  tracks, rather than copying the playing track's, which it still does
  elsewhere
- Ogg FLAC, Matroska, WMA, TrueAudio, AC-3, DTS, AU and the other audio
  files that GStreamer plays are read with their tags and covers, rather
  than turned away, as long as GStreamer has the plugins to play them: for
  WMA, the ugly plugins and gst-libav
- Lyrics, in the More menu of the player or with Ctrl+Shift+L, shows the
  lyrics of the playing track in the playlist pane: those of an LRC or a
  text file next to it with the same name, or else those in its tags.
  Synchronized lyrics, in the LRC format or in an MP3's SYLT frame, follow
  the music, with the line sung in the middle, and a click on a line, or
  Enter, plays from it. An album opened over the lyrics goes back to them
- Removing tracks from the playlist pane can be undone: the toast that
  says how many went has an Undo, which Ctrl+Z presses too. Tracks removed
  one after another while it's up come back together, each in its place
- Remove All can be undone the same way. The tracks come back with the
  order they had before they were shuffled, and the track that was
  playing comes back paused where it was
- Playing an album, or a track of an album, in place of the queue has an
  Undo too, when the queue had other tracks; playing another track of the
  same album doesn't need one
- The sort button in the playlist pane puts the playlist in order once,
  by album, artist, composer, title, year or file name, or reverses it.
  Composers go by their sort names, and a composer's works by catalogue
  number, as on the Works page. The movements of a work stay together, in
  their order. The toast's Undo, or Ctrl+Z, puts the order back. Sorting a
  shuffled playlist turns shuffle off, and Undo turns it back on

### Changed

- A new icon: the Katholische Hofkirche in Dresden, the church of the
  Catholic court that Zelenka wrote his church music for, in place of
  Charles Bridge in Prague
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
- Find cover files named cover, folder or front in any case and image
  type, such as `Cover.JPG` or `folder.webp`, Windows Media Player's
  `AlbumArt` files, and a folder's only image, whatever its name; look in
  the album's folder for tracks in a disc folder like `CD1`; and use cover
  files when the cover in the tags can't be decoded
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
- Update the Cornish translation
- Ctrl+S turns shuffling on and off, as in other music players, rather
  than Ctrl+R. Ctrl+R and F5 now scan the music folders for changes, as
  Reload does in other GNOME apps
- When there are no albums to show, the album page says which folder
  Dismas looked in, or that the music folders are missing or empty, as
  when their drive isn't plugged in, rather than only "No Albums". Its
  Choose Music Folders… button opens Preferences with Add Folder… ready
- Go to Album, in the menu of a track of the playlist and in the player's
  More menu, goes to the Albums page, where the albums scroll to the
  album, as Go to Work goes to the Works page, and opens the album in the
  playlist pane with the track flashing, as before. It's off in
  fullscreen
- The year of an album, which the grid can be sorted by, is when its
  tracks were recorded, if their tags say, or else when they were first
  released, rather than the year of a reissue
- Ctrl+P shows the playlists, rather than Ctrl+3, which shows the new
  Tracks page, and Ctrl+4 the new Works page, so that the shortcuts follow
  the order of the page buttons, and none is missing without works
- The music folders are read again once after updating, as the library
  now keeps the composers, the works and the performers of each track,
  when it was made, whether its work is an arrangement, and the other
  names of its works
- Scan the Music Folders moved from the main menu to Preferences, as Scan
  Now, as the music folders are watched
- A search of the albums shows the albums with a track that matches, and
  adding, playing or opening them takes the whole albums, rather than only
  the tracks that match, which the new Tracks page takes
- Composers written in one value, joined by " & " or " / ", as in "John
  Lennon & Paul McCartney", are each a composer of their own, in the
  Composers list, the works and the credits of the track
- The folders of an album's discs are also recognised with more after the
  number, as in "Disc 1 - Live" or "CD1 (Bonus)", and in a few other
  languages, as in "Disco 2" or "Disque 1", so that their tracks make one
  album without album tags, and find the cover in the album's folder
- Importing a playlist finds its tracks when it was made on another
  computer or on Windows, by the end of their paths, in the playlist's
  folder or in the music folders, whatever the case of their names
- Ctrl+F and typing go to the search of where the focus is: on the
  Albums, Tracks and Works pages, their own search, rather than the
  playlist's, and in the playlist pane, on the player and on the
  playlists, the playlist's. The cursor moves into the search with what's
  typed, and typing in the playlist goes on with a search that's open.
  Ctrl+F selects the text of the search, so that what's typed next starts
  a new search
- The desktop's media controls and the tray icon's tooltip name the
  composer of a classical track after the performers, or the performers
  after the composer, as Copy does, rather than only the artist; the
  artist stays first, for scrobblers that take only the first. Programs
  that read MPRIS also get the composers, the album artist, the lyricists,
  the genres, the track and disc numbers, the date and the file
- The icon of the track playing, or paused, is over its cover in the
  playlist pane and in an album's tracks, rather than at the end of the
  row, where every other row kept room for it, so the titles and the names
  under them have more room
- In the playlist pane and in an album's tracks, a long title, and the
  artist, the parts of a work or the performers under it, go on to a
  second line rather than being cut short after a few words in a narrow
  pane
- Removing the selected tracks from the playlist pane leaves selection
  mode, as the album grid's selection bar does once its buttons have taken
  the albums, rather than staying in it with no track selected. The
  keyboard focus goes to the track after the ones removed
- Scanning the music folders reads the tags of several files at a time,
  as the playlist already did, so a first scan, or the one after updating
  Dismas, takes less time

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
- `RUST_LOG` didn't change what Dismas logged, so the README's way of
  getting a debug log didn't work, and a release build, like the
  Flatpak's, couldn't give one
- Any program on the session bus could make Dismas read, overwrite or
  delete `.m3u8` files outside its playlists folder, through a playlist
  name such as `../x`
- Where an administrator had locked Dismas's settings with dconf, Dismas
  crashed when the window closed, when quitting, when switching playlists,
  when changing ReplayGain, or when the portal turned down Background
  Playback
- Other users of the computer could read the folders Dismas made for its
  data and its cache; the folders it makes now are private
- Playing an album from one of its tracks played the track after it when
  a file before it in the album couldn't be read
- Tracks in a folder whose name ends with a colon, such as "Live at
  Home:", were lost from playlists
- Ctrl+A in the album grid's search selected the albums rather than the
  text, so typing a new search added its words to the old ones
- Exporting a playlist to a slow device, such as a phone, froze the window
  until it was written
- A playlist name with braces in it, such as "My {n} list", came out
  changed in the messages that named it
- Searching the queue for "unknown" found the tracks without an artist
  tag, by the "Unknown artist" shown in its place
- With PulseAudio, a track paused before it had started playing, such as
  by a Pause from the desktop's media controls just after startup, or by a
  quick Play and Pause, stayed silent when played a few seconds later, or
  played with its time and waveform stuck where it started
- Esc did nothing with the cursor in an empty search of the album grid;
  it now hides the playlist pane beside the grid, as it does elsewhere in
  the window
- After hiding the playlist pane on the album page, typing didn't always
  search the albums, and Esc didn't always clear their search; the cursor
  goes to the album search now
- Enter on a row of the preferences, or of another dialog, played or paused
  the music, rather than turning its switch, picking its option or
  pressing its button
- Ctrl+F did nothing when the playlist's search had been left open and the
  playlist pane hidden; it shows the pane again, with the cursor in the
  search
- Screen readers didn't say which tracks of the queue or of an album, or
  which albums of the grid, were selected in selection mode
- The tracks of a long album could open below the name of their first
  work
- Ctrl+C in a search copied the details of the playing track, rather than
  the text selected
- On a touchscreen, a flick that started on the name of a work, over its
  movements, didn't scroll the list
- Screen readers found no name for the seek bar, the volume slider or the
  album search, read the time played as "Elapsed time" rather than the time,
  and read a playing icon on every track of the playlist
- MP3, AIFF, WAV and AAC files showed no record label
- MP3, AIFF, WAV and AAC files tagged by MusicBrainz Picard didn't show
  their soloists and other performers, only their conductor
- Tracks and folders behind symbolic links in the music folders, as in a
  library that beets links to, or in a folder that was opened or dropped,
  were left out
- An album without an album artist tag showed as by Various Artists as
  soon as one of its tracks had another artist, as with "feat."; it's by
  the artist of most of its tracks, if one has more than half of them
- The tracks of an album without track numbers played with "10" before
  "2", and the discs of an album in folders of their own, without disc
  numbers, played with their tracks mixed together: the first track of
  each disc, then the second of each
- Adding or dropping a folder, like the root of a drive, also added the
  tracks in its trash and other hidden folders, and the recycle bins and
  snapshots of network drives (`#recycle`, `#snapshot`, `@Recycle`) showed
  in the album grid
- An imported playlist written by a Windows player in its legacy encoding,
  as Winamp and foobar2000 write `.m3u` files, lost the tracks with accents
  in their names, and one with the line endings of old Macs lost them all
- Importing a playlist file whose name was a saved playlist's with a dot
  and a space before it, or a control character after it, like
  ". Road Trip.m3u", wrote over that playlist rather than making
  "Road Trip 2"
- A file in the music folders named as an audio file but holding an HLS,
  DASH or Smooth Streaming playlist made Dismas fetch the addresses in it
  when it read or played the file, outside the Flatpak
- Ctrl+F, while the playlist pane showed the tracks of an album, opened the
  playlist's search behind them rather than showing it
- Searching for a number found tracks by the times in their synchronized
  lyrics, such as "[01:12.50]" for "12"
- A click on the waveform, or a position set from the desktop's media
  controls, went to the whole second before it
- Starting Dismas by opening a file, as from a file manager, opened the
  playlist pane even when it had been hidden, and made the window wider by
  the pane, which it then kept the next time
- A player credited with an instrument whose name has parentheses of its
  own, as MusicBrainz names a drum kit "drums (drum set)", showed in Track
  Information with half the instrument in their name and a role ending in
  ")", and counted as a second player
- After an update that read the whole library again, tracks could count
  as added when their files were last written, rather than when they were
  first added, which changed the order of the albums sorted by when they
  were added: on drives that don't keep the time a file was made, and for
  files that a sync tool had replaced
- A cover file that was a symbolic link to one of the system's own files
  that say they're empty but aren't, such as `/proc/self/pagemap`, could
  fill the memory at every start
- A playlist that listed a named pipe, a special file that waits for a
  program to write to it, stopped loading there for good; it now keeps its
  place as a track that couldn't be read. A cover file or a CUE sheet
  swapped for one as the music folders were read could stop the scan the
  same way
- Importing or opening a playlist read the whole file, however large, so
  one of millions of short lines could fill the memory. A playlist from
  elsewhere over 32 MiB, or with more than 200,000 tracks, is now turned
  down
- Files opened from a file manager while the playlist of the last session
  was being read went among its tracks, rather than being added to it once
  it was read
- Saving the queue as a playlist right after startup, while its tracks
  were still being read, saved an empty playlist
- The scan forgot the tracks of a folder it couldn't read, like one on a
  network share that stopped answering, and those of a drive unplugged
  while it read them, and read them and made their covers again once they
  were back
- Scrolling over the volume slider with a touchpad on Wayland moved it a
  step for each pixel, so that a light scroll sent the volume to 0 or to
  100%
- Seeking to a time from another app, over MPRIS, in a track whose length
  couldn't be read went back to its start
- A file with tens of thousands of performers or composers in its tags,
  as only a crafted one has, made the player and its rows in the playlist
  take seconds to show it
- A file with many pictures that were slow to decode, as a crafted one can
  have, held up the tracks after it in the playlist, and the scan, for 10
  to 20 seconds for each picture
- A cover file added to the folder of an album, or replaced, showed in the
  album grid, but the player, the playlist and the album's tracks only
  showed it once Dismas was started again
- A cover replaced in the folder of an album while the album grid showed
  it kept the old cover in the grid
- A cover file moved or copied into the folder of an album wasn't noticed
  when it kept an older date, as moving a file does, and the album kept
  its old cover. A cover file that was removed wasn't noticed either. A
  cover file dated in the future, as on a drive written where the clock
  was ahead, made the album's cover again at every scan of the music
  folders

### Removed

- Ctrl+G, as Ctrl+2 shows the albums

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
