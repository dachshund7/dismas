// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};

use crate::tag_date::{DateKind, TagDate, TagDates};

use super::{
    MusicFolder, Roots, ThumbnailSizes, ThumbnailUpdate, is_mbid, path_bytes, path_from_bytes,
    shrink_thumbnail, thumbnail_name, thumbnail_size,
};

fn roots_of(paths: &[&Path]) -> Roots {
    let folders: Vec<MusicFolder> = paths
        .iter()
        .map(|path| MusicFolder::new(path.to_path_buf()))
        .collect();
    super::roots(&folders)
}

// Reads the music folders, as `scan()` does
fn scan_roots(
    conn: &mut rusqlite::Connection,
    roots: &Roots,
    sender: &async_channel::Sender<super::ScanEvent>,
) -> rusqlite::Result<(super::ScanResult, std::collections::HashSet<String>)> {
    use gtk::gio::prelude::FileExt;

    let trees = super::read_roots(roots);
    let unread: Vec<PathBuf> = trees
        .iter()
        .flat_map(|tree| tree.unread.iter().cloned())
        .collect();
    let paths = trees
        .into_iter()
        .flat_map(|tree| tree.files)
        .filter_map(|file| file.path())
        .collect();
    super::scan_with(conn, roots, paths, &unread, sender)
}

fn watch_list(folders: &[MusicFolder]) -> super::WatchList {
    let roots = super::roots(folders);
    super::folders_to_watch(folders, &roots, &super::read_roots(&roots))
}

fn test_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dismas-library-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(super::SCHEMA).unwrap();
    super::add_functions(&conn).unwrap();
    conn
}

#[test]
fn a_damaged_database_is_made_again() {
    let dir = test_dir("damaged");
    let path = dir.join("library.db");
    std::fs::write(&path, vec![0xa5; 8192]).unwrap();
    assert!(super::open_at(&path).is_err());

    let conn = super::open_or_recreate(&path).unwrap();
    let tracks: i64 = conn
        .query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))
        .unwrap();
    assert_eq!(tracks, 0);

    drop(conn);
    std::fs::remove_dir_all(&dir).unwrap();
}

// E.g. names from old Latin-1 rips
#[cfg(unix)]
#[test]
fn paths_that_are_not_utf8_round_trip() {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

    let path = Path::new(OsStr::from_bytes(b"/music/caf\xe9.flac"));
    assert_eq!(path_from_bytes(path_bytes(path).to_vec()), path);
}

// Like the queue, for which the title is the file name, and the album
// the folder's
#[test]
fn tracks_without_titles_are_searched_by_file_name() {
    use lofty::tag::{ItemKey, Tag, TagType};

    let path = Path::new("/music/Some Folder/01 - Intro.flac");
    let mut tag = Tag::new(TagType::VorbisComments);
    let tags = super::TrackTags::from_tag(
        &tag,
        path,
        Some(path),
        &super::ExtraComments::default(),
        (None, None),
    );
    assert_eq!(tags.album.as_deref(), Some("Some Folder"));
    let search = tags.search.unwrap();
    assert!(search.contains("intro"));
    assert!(search.contains("some folder"));

    tag.insert_text(ItemKey::AlbumTitle, "Album".to_string());
    let tags = super::TrackTags::from_tag(
        &tag,
        path,
        Some(path),
        &super::ExtraComments::default(),
        (None, None),
    );
    assert_eq!(tags.title, None);
    let search = tags.search.unwrap();
    assert!(search.contains("intro"));
    assert!(!search.contains("some folder"));

    tag.insert_text(ItemKey::TrackTitle, "Overture".to_string());
    let tags = super::TrackTags::from_tag(
        &tag,
        path,
        Some(path),
        &super::ExtraComments::default(),
        (None, None),
    );
    let search = tags.search.unwrap();
    assert!(search.contains("overture"));
    assert!(!search.contains("intro"));
}

#[test]
fn the_album_artist_of_a_track_that_is_not_a_movement_is_kept_as_an_artist() {
    use lofty::tag::{ItemKey, Tag, TagType};

    let path = Path::new("/music/Kinks/01.flac");
    let performers = |work: Option<(&str, &str)>| {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::AlbumArtist, "The Kinks".to_string());
        tag.insert_text(ItemKey::AlbumArtistSortOrder, "Kinks, The".to_string());
        tag.insert_text(ItemKey::TrackArtist, "The Kinks".to_string());
        if let Some((work, movement)) = work {
            tag.insert_text(ItemKey::Work, work.to_string());
            tag.insert_text(ItemKey::Movement, movement.to_string());
        }
        super::TrackTags::from_tag(
            &tag,
            path,
            Some(path),
            &super::ExtraComments::default(),
            (None, None),
        )
        .performers
    };
    assert_eq!(
        performers(None).as_deref(),
        Some("\na\tthe kinks\tThe Kinks\tKinks, The\t\n")
    );
    assert_eq!(performers(Some(("Arthur", "Victoria"))), None);
}

#[test]
fn works_are_told_apart_by_their_ids_or_names_and_composers() {
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::ExtraComments;

    let path = Path::new("/music/Requiem/01.flac");
    let work = |work: &str, composers: &[&str], extra: &ExtraComments| {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Work, work.to_string());
        for composer in composers {
            tag.push(lofty::tag::TagItem::new(
                ItemKey::Composer,
                lofty::tag::ItemValue::Text((*composer).to_string()),
            ));
        }
        super::TrackTags::from_tag(&tag, path, Some(path), extra, (None, None))
            .work
            .unwrap()
    };
    let none = ExtraComments::default();

    // Without an id, the same name and composers, ignoring case and
    // accents, are the same work, and other composers make another
    let requiem = work("Requiem, op. 89", &["Antonín Dvořák"], &none);
    assert_eq!(
        requiem.key,
        work("requiem, Op. 89", &["Antonin Dvorak"], &none).key
    );
    assert_ne!(
        requiem.key,
        work("Requiem, op. 89", &["Someone Else"], &none).key
    );
    assert_eq!(requiem.name, "Requiem, op. 89");
    assert_eq!(
        requiem.composers.as_deref(),
        Some("\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n")
    );
    // A whole work has no order
    assert_eq!(requiem.order, None);

    // The hierarchy's id, whatever the work tag says, and the place of
    // the movement
    let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0,
            "path": [
                { "id": "1d6a0d5c-0000-0000-0000-000000000000", "title": "Requiem in B-flat minor",
                  "name": "Requiem in B-flat minor",
                  "composers": [{ "id": "a", "name": "Antonín Dvořák" }] },
                { "id": "s", "title": "…: Sequentia", "name": "Sequentia", "position": 3 },
                { "id": "d", "title": "…: Dies irae", "name": "Dies irae", "position": 1 },
            ] }] });
    let extra = ExtraComments::with_work_hierarchy(&hierarchy.to_string());
    let movement = work("Requiem v B-moll, op. 89", &["Antonín Dvořák"], &extra);
    assert_eq!(movement.key, "1d6a0d5c-0000-0000-0000-000000000000");
    assert_eq!(movement.name, "Requiem in B-flat minor");
    assert_eq!(movement.order.as_deref(), Some("3.1"));
    assert_eq!(movement.part_ids.as_deref(), Some("s\nd"));
}

#[test]
fn a_whole_work_in_a_track_is_kept_as_an_arrangement() {
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::ExtraComments;

    // A prelude for the lute played on the harpsichord, which MusicBrainz
    // links to the catch-all for its arrangements, whole in one track
    const CATCH_ALL: &str = "18047db5-0000-0000-0000-000000000000";
    const ORIGINAL: &str = "d53cc638-0000-0000-0000-000000000000";
    let path = Path::new("/music/Keyboard/12.flac");
    let prelude = "Prelude in C minor, BWV 999";
    let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0,
            "path": [{ "id": CATCH_ALL, "title": prelude, "name": prelude,
                       "disambiguation": "catch-all for arrangements",
                       "arrangement_of": { "id": ORIGINAL, "title": prelude } }] }] });
    let extra = ExtraComments::with_work_hierarchy(&hierarchy.to_string());
    let work = |work_id: &str| {
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Work, prelude.to_string());
        tag.insert_text(ItemKey::MusicBrainzWorkId, work_id.to_string());
        super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None))
            .work
            .unwrap()
    };

    let arrangement = work(CATCH_ALL);
    assert_eq!(arrangement.key, CATCH_ALL);
    assert_eq!(arrangement.arrangement_of.as_deref(), Some(prelude));
    assert_eq!(arrangement.arrangement.as_deref(), Some(""));
    assert_eq!(arrangement.arrangement_of_id.as_deref(), Some(ORIGINAL));
    assert_eq!(arrangement.part_ids, None);
    // Not when the tags name another work than the plugin
    assert_eq!(work(ORIGINAL).arrangement_of, None);
}

#[test]
fn an_arranged_movement_is_kept_by_its_own_id() {
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::ExtraComments;

    // A song of a cycle arranged for a piano trio, which MusicBrainz
    // links to the catch-all for the song's arrangements, whose title
    // the plugin splits into the work and movement tags
    const CATCH_ALL: &str = "6d1b4a3e-0000-0000-0000-000000000000";
    const SONG: &str = "c0f3a2b1-0000-0000-0000-000000000000";
    let path = Path::new("/music/Trios/13.flac");
    let title = "Schwanengesang, D. 957: IV. Ständchen";
    let tag_of = |arrangement_of: serde_json::Value| {
        let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0,
                "path": [{ "id": CATCH_ALL, "title": title, "name": title,
                           "disambiguation": "catch-all for arrangements",
                           "arrangement_of": arrangement_of }] }] });
        let extra = ExtraComments::with_work_hierarchy(&hierarchy.to_string());
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Work, "Schwanengesang, D. 957".to_string());
        tag.insert_text(ItemKey::Movement, "Ständchen".to_string());
        tag.insert_text(ItemKey::MovementNumber, "4".to_string());
        tag.insert_text(ItemKey::Composer, "Franz Schubert".to_string());
        tag.insert_text(ItemKey::MusicBrainzWorkId, CATCH_ALL.to_string());
        super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None))
            .work
            .unwrap()
    };

    let arranged = tag_of(serde_json::json!({ "id": SONG, "title": title }));
    assert_eq!(arranged.key, CATCH_ALL);
    assert_eq!(arranged.name, "Schwanengesang, D. 957");
    assert_eq!(arranged.order.as_deref(), Some("4"));
    assert_eq!(arranged.arrangement_of.as_deref(), Some(title));
    assert_eq!(arranged.arrangement.as_deref(), Some(""));
    assert_eq!(arranged.arrangement_of_id.as_deref(), Some(SONG));

    // A movement that MusicBrainz doesn't link to its work, but that
    // arranges nothing, is still one with the work of the same name
    let movement = tag_of(serde_json::Value::Null);
    assert_eq!(movement.arrangement_of, None);
    assert!(
        movement.key.starts_with("t:schwanengesang"),
        "{}",
        movement.key
    );
}

#[test]
fn performers_are_kept_with_their_kinds_and_roles() {
    use crate::audio::{ListedPerformer, PerformerKind};

    let performer = |kind, name: &str, role: Option<&str>| ListedPerformer {
        kind,
        name: name.to_string(),
        role: role.map(ToString::to_string),
        sort: None,
    };
    assert_eq!(super::performer_entries(&[]), None);
    assert_eq!(
        super::performer_entries(&[
            performer(PerformerKind::Conductor, "Philippe Herreweghe", None),
            performer(
                PerformerKind::Ensemble,
                "Collegium Vocale Gent",
                Some("choir vocals")
            ),
            performer(
                PerformerKind::Soloist,
                "Ilse Eerens",
                Some("soprano vocals")
            ),
        ])
        .as_deref(),
        Some(
            "\nc\tphilippe herreweghe\tPhilippe Herreweghe\tHerreweghe, Philippe\t\n\
                 e\tcollegium vocale gent\tCollegium Vocale Gent\tCollegium Vocale Gent\t\
                 choir vocals\n\
                 s\tilse eerens\tIlse Eerens\tEerens, Ilse\tsoprano vocals\n"
        )
    );

    // An artist is sorted by the sort name its tags give it, or else by
    // its name, never turned around
    let artist = |name: &str, sort: Option<&str>| ListedPerformer {
        kind: PerformerKind::Artist,
        name: name.to_string(),
        role: None,
        sort: sort.map(ToString::to_string),
    };
    assert_eq!(
        super::performer_entries(&[
            artist("The Kinks", Some("Kinks, The")),
            artist("Pink Floyd", None),
        ])
        .as_deref(),
        Some(
            "\na\tthe kinks\tThe Kinks\tKinks, The\t\n\
                 a\tpink floyd\tPink Floyd\tPink Floyd\t\n"
        )
    );
}

#[test]
fn tracks_are_searched_by_the_names_in_the_work_hierarchy() {
    use lofty::tag::{ItemKey, Tag, TagType};

    let path = Path::new("/music/WTC/01.flac");
    let mut tag = Tag::new(TagType::VorbisComments);
    tag.insert_text(
        ItemKey::Work,
        "Präludium und Fuge C-Dur, BWV 846".to_string(),
    );
    let hierarchy = serde_json::json!({ "version": 1, "segments": [{ "work_index": 1,
            "path": [
                { "id": "b", "title": "The Well-Tempered Clavier, Book I", "name": "Book I" },
                { "id": "p", "title": "…: Prelude and Fugue in C major, BWV 846",
                  "name": "Prelude and Fugue in C major, BWV 846", "number": 1 },
                { "id": "f", "title": "…: Fugue", "name": "Fugue", "number": 2 },
            ] }] });
    let extra = super::ExtraComments::with_work_hierarchy(&hierarchy.to_string());
    let search = super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None))
        .search
        .unwrap();
    assert!(search.contains("praludium und fuge"));
    assert!(search.contains("well-tempered clavier"));
    assert!(search.contains("prelude and fugue in c major"));
}

#[test]
fn disc_folders_have_names_in_several_languages() {
    use super::is_disc_folder;
    use std::ffi::OsStr;

    for name in [
        "CD1",
        "CD 2",
        "cd_03",
        "Disc 1",
        "Disc 1 - Live at the Proms",
        "Disc 2 of 2",
        "CD1 (Bonus)",
        "Disk.4",
        "Disco 2",
        "Disque 1",
        "Schijf 2",
        "Płyta 1",
        "CD 153",
    ] {
        assert!(is_disc_folder(OsStr::new(name)), "{name}");
    }
    for name in [
        "CD Collection",
        "Discography",
        "Disco 1980",
        "Disco 1980 Hits",
        "CD1s",
        "Disc One",
        "Album",
        "1",
    ] {
        assert!(!is_disc_folder(OsStr::new(name)), "{name}");
    }
}

#[test]
fn album_folder_entry_is_the_disc_folder_or_the_file() {
    use super::album_folder_entry;

    // A track is taken for a sheet's only while the sheet is there
    let dir = test_dir("album-folder-entry");
    std::fs::create_dir(dir.join("CD 2")).unwrap();
    for name in ["Album.cue", "Album.flac", "CD 2/Album.cue"] {
        std::fs::write(dir.join(name), b"").unwrap();
    }

    for (track, entry) in [
        ("01.flac", "01.flac"),
        ("CD 1/01.flac", "CD 1"),
        ("Album.cue/track03", "Album.cue"),
        ("Album.flac/track03", "Album.flac"),
        ("CD 2/Album.cue/track01", "CD 2"),
    ] {
        assert_eq!(
            album_folder_entry(&dir.join(track)),
            dir.join(entry),
            "{track}"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tracks_without_an_album_are_grouped_by_folder() {
    use super::query_albums;
    use crate::utils::write_test_wav;

    let music = test_dir("folder-albums");
    std::fs::create_dir_all(music.join("Album/CD 2")).unwrap();
    std::fs::create_dir_all(music.join("Sheet")).unwrap();
    write_test_wav(&music.join("Loose.wav"), 1);
    write_test_wav(&music.join("Album/One.wav"), 1);
    write_test_wav(&music.join("Album/CD 2/Two.wav"), 1);
    write_test_wav(&music.join("Sheet/Sheet.wav"), 4);
    std::fs::write(
        music.join("Sheet/Sheet.cue"),
        "FILE \"Sheet.wav\" WAVE\n\
             TRACK 01 AUDIO\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:02:00\n",
    )
    .unwrap();

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();

    // The discs of an album in folders of their own are one album, and
    // the tracks in the music folder itself are named after it
    let root = music.file_name().unwrap().to_string_lossy().into_owned();
    let mut albums: Vec<(String, String)> = query_albums(&conn, &[])
        .unwrap()
        .into_iter()
        .map(|album| (album.title, album.artist))
        .collect();
    albums.sort();
    let mut expected: Vec<(String, String)> = ["Album", "Sheet", &root]
        .iter()
        .map(|title| (title.to_string(), "Unknown artist".to_string()))
        .collect();
    expected.sort();
    assert_eq!(albums, expected);

    let mut stmt = conn
        .prepare("SELECT path, album_id, search FROM tracks ORDER BY path")
        .unwrap();
    let rows: Vec<(Vec<u8>, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let album_of = |name: &str| {
        let path = path_bytes(&music.join(name)).to_vec();
        rows.iter().find(|row| row.0 == path).unwrap().1.clone()
    };
    assert_eq!(album_of("Album/One.wav"), album_of("Album/CD 2/Two.wav"));
    assert_eq!(
        album_of("Sheet/Sheet.cue/track01"),
        album_of("Sheet/Sheet.cue/track02")
    );
    assert_ne!(album_of("Album/One.wav"), album_of("Loose.wav"));

    // The folder is searched like a tag of the album
    let two = path_bytes(&music.join("Album/CD 2/Two.wav")).to_vec();
    let search = &rows.iter().find(|row| row.0 == two).unwrap().2;
    assert!(search.contains("album"));
    assert!(search.contains("two"));

    std::fs::remove_dir_all(&music).unwrap();
}

#[test]
fn searching_counts_the_matching_tracks_of_each_album() {
    let conn = test_db();
    let tracks: [(&str, &str, Option<&str>); 6] = [
        ("/music/mass/1.opus", "mass", Some("kyrie soprano gardiner")),
        ("/music/mass/2.opus", "mass", Some("gloria gardiner")),
        ("/music/mass/3.opus", "mass", None),
        (
            "/music/passion/1.opus",
            "passion",
            Some("aria soprano gardiner"),
        ),
        ("/music/unmounted/1.opus", "passion", Some("aria gardiner")),
        ("/music/songs/1.opus", "songs", Some("lied tenor")),
    ];
    for (path, album_id, search) in tracks {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
            rusqlite::params![path.as_bytes(), album_id, search],
        )
        .unwrap();
    }
    let search = |words: &[&str]| {
        let query = super::TrackFilter::query(&words.join(" "));
        let hidden = [PathBuf::from("/music/unmounted")];
        let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(
            &conn,
            &super::TrackFilter {
                query,
                ..Default::default()
            },
            &hidden,
        )
        .unwrap()
        .into_iter()
        .collect();
        albums.sort();
        albums
    };

    // The track without search text counts towards its album only, and
    // the one in a folder that isn't there doesn't count at all
    assert_eq!(
        search(&["gardiner"]),
        [
            ("mass".to_string(), (2, 3)),
            ("passion".to_string(), (1, 1))
        ]
    );
    assert_eq!(
        search(&["soprano", "gardiner"]),
        [
            ("mass".to_string(), (1, 3)),
            ("passion".to_string(), (1, 1))
        ]
    );
    assert_eq!(search(&["tenor"]), [("songs".to_string(), (1, 1))]);
    assert!(search(&["bass"]).is_empty());
}

#[test]
fn only_uuids_are_musicbrainz_ids() {
    assert!(is_mbid("9e8f8d6e-3a1c-4b4f-9b0c-2f1d6a5e7c3b"));
    assert!(!is_mbid(""));
    assert!(!is_mbid("../../Pictures/photo"));
    assert!(!is_mbid("/home/user/Pictures/photo"));
}

// The grid's covers were PNG files of 480 pixels; made for another
// size, or in the old format, they're made again
#[test]
fn thumbnails_are_named_by_size_and_format() {
    let dir = Path::new("/cache/album-covers");
    assert_eq!(thumbnail_name("album", 240, false), "album-240.jpg");
    assert_eq!(thumbnail_name("album", 480, true), "album-480-alpha.png");

    let size = |name: &str| thumbnail_size(&dir.join(name), dir, "album");
    assert_eq!(size("album-240.jpg"), Some(240));
    assert_eq!(size("album-480-alpha.png"), Some(480));
    // The PNG files of older versions
    assert_eq!(size("album-480.png"), None);
    assert_eq!(size("other-240.jpg"), None);
    assert_eq!(size("album-other-240.jpg"), None);
    assert_eq!(size("album-.jpg"), None);
    assert_eq!(
        thumbnail_size(Path::new("/elsewhere/album-240.jpg"), dir, "album"),
        None
    );
}

// The thumbnails follow the zoom: made again when the covers are zoomed
// in, made smaller when they come back, and kept when they were made for
// a monitor at a scale of 2
#[test]
fn thumbnails_follow_the_zoom() {
    use ThumbnailUpdate::{Keep, Remake, Shrink};

    // At the normal size: 240 pixels at a scale of 1, and 480 at 2
    let scale_1 = ThumbnailSizes {
        wanted: 240,
        scale_2: 480,
    };
    let scale_2 = ThumbnailSizes {
        wanted: 480,
        scale_2: 480,
    };
    assert_eq!(scale_1.update(Some(240)), Keep);
    assert_eq!(scale_2.update(Some(480)), Keep);
    // Made while a monitor at a scale of 2 was connected
    assert_eq!(scale_1.update(Some(480)), Keep);
    // Made while the covers were zoomed in
    assert_eq!(scale_1.update(Some(320)), Shrink(240));
    assert_eq!(scale_1.update(Some(640)), Shrink(480));
    assert_eq!(scale_2.update(Some(640)), Shrink(480));
    // Too small, or in the format of older versions
    assert_eq!(scale_2.update(Some(240)), Remake);
    assert_eq!(scale_2.update(None), Remake);

    // Zoomed in, at a scale of 2
    let zoomed = ThumbnailSizes {
        wanted: 640,
        scale_2: 640,
    };
    assert_eq!(zoomed.update(Some(480)), Remake);
    assert_eq!(zoomed.update(Some(640)), Keep);
}

// A thumbnail made smaller keeps its shape, and its format: a JPEG, or a
// PNG for one with transparency
#[test]
fn thumbnails_are_made_smaller_from_larger_ones() {
    let dir = gtk::glib::mkdtemp("dismas-shrink-XXXXXX").unwrap();
    let jpeg = dir.join("album-640.jpg");
    image::RgbImage::from_pixel(640, 320, image::Rgb([200, 100, 50]))
        .save(&jpeg)
        .unwrap();
    let png = dir.join("other-640-alpha.png");
    image::RgbaImage::from_pixel(640, 640, image::Rgba([200, 100, 50, 128]))
        .save(&png)
        .unwrap();

    let made = shrink_thumbnail(&jpeg, &dir, "album", 480).unwrap();
    assert_eq!(made, dir.join("album-480.jpg"));
    assert_eq!(image::image_dimensions(&made).unwrap(), (480, 240));

    let made = shrink_thumbnail(&png, &dir, "other", 480).unwrap();
    assert_eq!(made, dir.join("other-480-alpha.png"));
    assert_eq!(image::image_dimensions(&made).unwrap(), (480, 480));

    assert!(shrink_thumbnail(&dir.join("missing.jpg"), &dir, "gone", 480).is_none());
    for name in [
        "album-640.jpg",
        "album-480.jpg",
        "other-640-alpha.png",
        "other-480-alpha.png",
    ] {
        std::fs::remove_file(dir.join(name)).unwrap();
    }
    std::fs::remove_dir(&dir).unwrap();
}

// A cover file added next to the tracks of an album makes the scan list
// all its tracks, so that the queue looks for their cover again, once,
// even when it can't be read. An album that's new to the library with a
// cover file has its tracks listed apart, as the queue only looks again
// for the cover of those it has without one, which reached it before
// their cover file
#[test]
fn a_cover_file_added_to_an_album_lists_its_tracks() {
    use crate::utils::write_test_wav;

    let music = test_dir("cover-added");
    let covers = test_dir("cover-added-thumbnails");
    for folder in ["Plain", "Unreadable", "Covered", "Discs/CD1", "Discs/CD2"] {
        std::fs::create_dir_all(music.join(folder)).unwrap();
    }
    let red = image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0]));
    red.save(music.join("Covered/cover.png")).unwrap();
    let plain = music.join("Plain/1.wav");
    let first_disc = music.join("Discs/CD1/1.wav");
    let second_disc = music.join("Discs/CD2/1.wav");
    let unreadable = music.join("Unreadable/1.wav");
    for track in [&plain, &first_disc, &second_disc, &unreadable] {
        write_test_wav(track, 1);
    }
    let covered = music.join("Covered/1.wav");
    write_test_wav(&covered, 1);

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let roots = roots_of(&[&music]);
    let sizes = ThumbnailSizes {
        wanted: 240,
        scale_2: 480,
    };
    // The tracks listed, and those of new albums
    let mut scan = || {
        let (_, stale) = scan_roots(&mut conn, &roots, &sender).unwrap();
        let mut made =
            super::update_covers(&mut conn, &covers, &stale, &[], sizes, &sender).unwrap();
        made.tracks_with_new_cover.sort();
        (made.tracks_with_new_cover, made.tracks_of_new_albums)
    };
    let none = Vec::<PathBuf>::new;
    assert_eq!(scan(), (none(), vec![covered]));

    red.save(music.join("Plain/cover.png")).unwrap();
    red.save(music.join("Discs/folder.png")).unwrap();
    std::fs::write(music.join("Unreadable/cover.jpg"), b"not an image").unwrap();
    assert_eq!(
        scan(),
        (
            vec![first_disc.clone(), second_disc.clone(), plain, unreadable],
            none()
        )
    );
    // Once made, and while the one that can't be read is tried again
    assert_eq!(scan(), (none(), none()));

    // The tracks of another disc folder than the first track's show the
    // cover files in theirs, and the same file moved to the first one
    // shows on the other tracks
    red.save(music.join("Discs/CD2/cover.png")).unwrap();
    let discs = vec![first_disc, second_disc];
    assert_eq!(scan(), (discs.clone(), none()));
    std::fs::rename(
        music.join("Discs/CD2/cover.png"),
        music.join("Discs/CD1/cover.png"),
    )
    .unwrap();
    assert_eq!(scan(), (discs, none()));
    assert_eq!(scan(), (none(), none()));

    // A disc folder that only shows the cover files of the album's folder,
    // as the first one does, can come and go without a change to them
    std::fs::create_dir_all(music.join("Discs/CD3")).unwrap();
    write_test_wav(&music.join("Discs/CD3/1.wav"), 1);
    assert_eq!(scan(), (none(), none()));
    std::fs::remove_dir_all(music.join("Discs/CD2")).unwrap();
    assert_eq!(scan(), (none(), none()));

    std::fs::remove_dir_all(&music).unwrap();
    std::fs::remove_dir_all(&covers).unwrap();
}

// A cover file changed next to an album's tracks is noticed whatever its
// time: one replaced by an older file, as moving a file in keeps its
// time, one dated in the future, only once, one replaced while the
// thumbnail was gone from the cache, and one removed
#[test]
fn a_changed_cover_file_is_noticed_whatever_its_time() {
    use std::time::{Duration, SystemTime};

    use crate::utils::write_test_wav;

    let music = test_dir("cover-times");
    let covers = test_dir("cover-times-thumbnails");
    let track = music.join("1.wav");
    write_test_wav(&track, 1);
    let cover = music.join("cover.png");
    let save_cover = |color, time| {
        image::RgbImage::from_pixel(8, 8, image::Rgb(color))
            .save(&cover)
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&cover)
            .unwrap()
            .set_modified(time)
            .unwrap();
    };
    let now = SystemTime::now();
    let day = Duration::from_secs(24 * 60 * 60);

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let roots = roots_of(&[&music]);
    let sizes = ThumbnailSizes {
        wanted: 240,
        scale_2: 480,
    };
    let mut scan = || {
        let (_, stale) = scan_roots(&mut conn, &roots, &sender).unwrap();
        let made = super::update_covers(&mut conn, &covers, &stale, &[], sizes, &sender).unwrap();
        // The album is new at first, which lists its tracks apart
        let listed = [made.tracks_with_new_cover, made.tracks_of_new_albums].concat();
        (made.count, listed)
    };
    let listed = vec![track];

    save_cover([255, 0, 0], now);
    assert_eq!(scan(), (1, listed.clone()));
    save_cover([0, 255, 0], now - 7 * day);
    assert_eq!(scan(), (1, listed.clone()));
    save_cover([0, 0, 255], now + day);
    assert_eq!(scan(), (1, listed.clone()));
    assert_eq!(scan(), (0, vec![]));
    std::fs::remove_dir_all(&covers).unwrap();
    save_cover([255, 255, 0], now);
    assert_eq!(scan(), (1, listed.clone()));
    std::fs::remove_file(&cover).unwrap();
    assert_eq!(scan(), (0, listed));
    assert_eq!(scan(), (0, vec![]));

    std::fs::remove_dir_all(&music).unwrap();
    std::fs::remove_dir_all(&covers).unwrap();
}

// The covers outlive the versions of the schema, so the key of the cover
// files is added to the covers of an older version, which keep their
// thumbnails, and the next scan fills it in without making them again,
// but for a cover file replaced while the older version wasn't running,
// which that version would have noticed by its time
#[test]
fn a_new_version_of_the_schema_adds_the_key_of_the_cover_files() {
    use std::time::{Duration, SystemTime};

    use crate::utils::write_test_wav;

    let music = test_dir("cover-key-music");
    let covers = test_dir("cover-key-thumbnails");
    let save_cover = |folder: &str, color| {
        image::RgbImage::from_pixel(8, 8, image::Rgb(color))
            .save(music.join(folder).join("cover.png"))
            .unwrap();
    };
    let replaced = music.join("Replaced/1.wav");
    for folder in ["Kept", "Replaced"] {
        std::fs::create_dir_all(music.join(folder)).unwrap();
        write_test_wav(&music.join(folder).join("1.wav"), 1);
        save_cover(folder, [255, 0, 0]);
    }
    let dir = test_dir("cover-key-db");
    let path = dir.join("library.db");
    let (sender, _receiver) = async_channel::unbounded();
    let roots = roots_of(&[&music]);
    let sizes = ThumbnailSizes {
        wanted: 240,
        scale_2: 480,
    };
    let keys = |conn: &rusqlite::Connection| -> Vec<Option<String>> {
        conn.prepare("SELECT cover_files FROM covers")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };

    let mut conn = super::open_at(&path).unwrap();
    let (_, stale) = scan_roots(&mut conn, &roots, &sender).unwrap();
    super::update_covers(&mut conn, &covers, &stale, &[], sizes, &sender).unwrap();
    // As if an older version had made the database, without the column;
    // the table is made as it made it, as `CREATE TABLE … AS` would leave
    // out the primary key that the scan's `INSERT OR REPLACE` goes by
    conn.execute_batch(
        "CREATE TABLE old_covers (album_id TEXT PRIMARY KEY, path TEXT);
         INSERT INTO old_covers SELECT album_id, path FROM covers;
         DROP TABLE covers;
         ALTER TABLE old_covers RENAME TO covers;",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", super::SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);
    // Newer than its thumbnail, which was just made
    save_cover("Replaced", [0, 0, 255]);
    std::fs::File::options()
        .write(true)
        .open(music.join("Replaced/cover.png"))
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(60 * 60))
        .unwrap();

    let mut conn = super::open_at(&path).unwrap();
    assert_eq!(keys(&conn), [None, None]);
    let (_, stale) = scan_roots(&mut conn, &roots, &sender).unwrap();
    let made = super::update_covers(&mut conn, &covers, &stale, &[], sizes, &sender).unwrap();
    assert_eq!(
        (made.count, made.tracks_with_new_cover),
        (1, vec![replaced])
    );
    assert!(
        keys(&conn)
            .iter()
            .all(|key| key.as_ref().is_some_and(|key| !key.is_empty()))
    );
    // Once the keys are kept, the times no longer count
    let (_, stale) = scan_roots(&mut conn, &roots, &sender).unwrap();
    let made = super::update_covers(&mut conn, &covers, &stale, &[], sizes, &sender).unwrap();
    assert_eq!((made.count, made.tracks_with_new_cover), (0, vec![]));

    std::fs::remove_dir_all(&music).unwrap();
    std::fs::remove_dir_all(&covers).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

// A track in the database, with paths relative to a folder
#[derive(Debug, PartialEq)]
struct Row {
    path: String,
    file: String,
    track: Option<u32>,
    title: Option<String>,
    added: i64,
}

fn tracks(conn: &rusqlite::Connection, music: &std::path::Path) -> Vec<Row> {
    let relative = |bytes: Vec<u8>| {
        path_from_bytes(bytes)
            .strip_prefix(music)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    let mut stmt = conn
        .prepare("SELECT path, file, track, title, added FROM tracks ORDER BY path")
        .unwrap();
    stmt.query_map([], |row| {
        Ok(Row {
            path: relative(row.get(0)?),
            file: relative(row.get(1)?),
            track: row.get(2)?,
            title: row.get(3)?,
            added: row.get(4)?,
        })
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

// As in a folder on a network share that stops answering: the files
// that couldn't be listed can still be there
#[cfg(unix)]
#[test]
fn the_tracks_of_a_folder_that_cannot_be_read_are_kept() {
    use std::os::unix::fs::PermissionsExt;

    use crate::utils::write_test_wav;

    // Root reads any folder, as in the containers of CI
    // SAFETY: geteuid has no preconditions
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let music = test_dir("unread");
    let locked = music.join("Locked");
    std::fs::create_dir_all(&locked).unwrap();
    write_test_wav(&music.join("Gone.wav"), 1);
    write_test_wav(&music.join("Open.wav"), 1);
    write_test_wav(&locked.join("Locked.wav"), 1);
    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let roots = roots_of(&[&music]);
    scan_roots(&mut conn, &roots, &sender).unwrap();
    assert_eq!(tracks(&conn, &music).len(), 3);

    // A file that's gone is still forgotten
    std::fs::remove_file(music.join("Gone.wav")).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let scanned = scan_roots(&mut conn, &roots, &sender);
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    scanned.unwrap();
    let paths: Vec<String> = tracks(&conn, &music)
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(paths, ["Locked/Locked.wav", "Open.wav"]);

    std::fs::remove_dir_all(&music).unwrap();
}

// As a drive unplugged while the scan reads it: the folders were read,
// and then the files are gone
#[test]
fn the_tracks_of_a_music_folder_that_goes_while_it_is_read_are_kept() {
    use gtk::gio::prelude::FileExt;

    use crate::utils::write_test_wav;

    let music = test_dir("going-music");
    let drive = test_dir("going-drive");
    write_test_wav(&music.join("Here.wav"), 1);
    write_test_wav(&drive.join("Away.wav"), 1);
    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let count = |conn: &rusqlite::Connection| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))
            .unwrap()
    };
    let roots = roots_of(&[&music, &drive]);
    scan_roots(&mut conn, &roots, &sender).unwrap();
    assert_eq!(count(&conn), 2);

    let paths = super::read_roots(&roots)
        .into_iter()
        .flat_map(|tree| tree.files)
        .filter_map(|file| file.path())
        .collect();
    std::fs::remove_dir_all(&drive).unwrap();
    super::scan_with(&mut conn, &roots, paths, &[], &sender).unwrap();
    assert_eq!(count(&conn), 2);

    std::fs::remove_dir_all(&music).unwrap();
}

#[test]
fn the_tracks_of_a_cue_sheet_take_the_place_of_its_file() {
    use crate::utils::write_test_wav;

    let music = test_dir("cue");
    std::fs::create_dir_all(music.join("Album")).unwrap();
    std::fs::create_dir_all(music.join("Other")).unwrap();
    write_test_wav(&music.join("Album/Album.wav"), 10);
    write_test_wav(&music.join("Other/Song.wav"), 1);
    let cue = music.join("Album/Album.cue");
    std::fs::write(
        &cue,
        "TITLE \"Album\"\nFILE \"Album.wav\" WAVE\n\
             TRACK 01 AUDIO\nTITLE \"One\"\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nTITLE \"Two\"\nINDEX 01 00:03:00\n",
    )
    .unwrap();

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();

    let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    assert_eq!(result.n_changed, 3);
    let found = tracks(&conn, &music);
    let files: Vec<_> = found
        .iter()
        .map(|row| (row.path.as_str(), row.file.as_str()))
        .collect();
    assert_eq!(
        files,
        vec![
            ("Album/Album.cue/track01", "Album/Album.cue"),
            ("Album/Album.cue/track02", "Album/Album.cue"),
            ("Other/Song.wav", "Other/Song.wav"),
        ]
    );
    let titles: Vec<_> = found
        .iter()
        .map(|row| (row.track, row.title.as_deref()))
        .collect();
    assert_eq!(
        titles,
        vec![(Some(1), Some("One")), (Some(2), Some("Two")), (None, None)]
    );
    let added = found[0].added;

    // Nothing changed
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    assert_eq!(result.n_changed, 0);

    // Without the sheet, the file is a track again, added when its
    // tracks were
    std::fs::remove_file(&cue).unwrap();
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    assert_eq!(result.n_changed, 3);
    let found = tracks(&conn, &music);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].path, "Album/Album.wav");
    assert_eq!(found[0].file, "Album/Album.wav");
    assert_eq!(found[0].added, added);

    std::fs::remove_dir_all(&music).unwrap();
}

#[test]
fn a_new_version_of_the_schema_keeps_when_the_tracks_were_added() {
    use crate::utils::write_test_wav;

    let music = test_dir("added-music");
    let away = test_dir("added-away");
    write_test_wav(&music.join("Kept.wav"), 1);
    write_test_wav(&music.join("Gone.wav"), 1);
    write_test_wav(&away.join("Away.wav"), 1);
    let dir = test_dir("added-db");
    let path = dir.join("library.db");
    let (sender, _receiver) = async_channel::unbounded();
    let kept = |conn: &rusqlite::Connection| -> i64 {
        conn.query_row("SELECT COUNT(*) FROM added_times", [], |row| row.get(0))
            .unwrap()
    };

    let mut conn = super::open_at(&path).unwrap();
    scan_roots(&mut conn, &roots_of(&[&music, &away]), &sender).unwrap();
    // As if they had been added long ago, and an older version had made
    // the database
    conn.execute("UPDATE tracks SET added = 100", []).unwrap();
    conn.pragma_update(None, "user_version", super::SCHEMA_VERSION - 1)
        .unwrap();
    drop(conn);

    let mut conn = super::open_at(&path).unwrap();
    assert!(tracks(&conn, &music).is_empty());
    assert_eq!(kept(&conn), 3);
    // A file that's gone is forgotten, and a folder that isn't there
    // keeps its times until it's back
    std::fs::remove_file(music.join("Gone.wav")).unwrap();
    write_test_wav(&music.join("New.wav"), 1);
    std::fs::remove_dir_all(&away).unwrap();
    scan_roots(&mut conn, &roots_of(&[&music, &away]), &sender).unwrap();
    let found = tracks(&conn, &music);
    let added: Vec<_> = found
        .iter()
        .map(|row| (row.path.as_str(), row.added))
        .collect();
    assert_eq!(added.len(), 2);
    assert_eq!(added[0], ("Kept.wav", 100));
    assert_eq!(added[1].0, "New.wav");
    assert!(added[1].1 > 100);
    assert_eq!(kept(&conn), 1);

    std::fs::create_dir_all(&away).unwrap();
    write_test_wav(&away.join("Away.wav"), 1);
    scan_roots(&mut conn, &roots_of(&[&music, &away]), &sender).unwrap();
    let back: i64 = conn
        .query_row(
            "SELECT added FROM tracks WHERE path = ?1",
            [path_bytes(&away.join("Away.wav"))],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(back, 100);
    assert_eq!(kept(&conn), 0);

    drop(conn);
    for dir in [&music, &away, &dir] {
        std::fs::remove_dir_all(dir).unwrap();
    }
}

// Times a first scan of a few thousand tagged tracks, which are in the
// temporary folder, so it's the reading of the tags that's timed more
// than the disk:
//   cargo test --release time_scan -- --ignored --nocapture
#[test]
#[ignore]
fn time_scan() {
    let (music, files) = tagged_test_files("time-scan");
    for album in 0..150 {
        let dir = music.join(format!("Album {album}"));
        std::fs::create_dir_all(&dir).unwrap();
        for track in 0..14 {
            std::fs::copy(&files[0], dir.join(format!("{track:02}.wav"))).unwrap();
        }
    }
    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let start = std::time::Instant::now();
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    println!(
        "scan of {} tracks: {:.0} ms",
        result.n_changed,
        start.elapsed().as_secs_f64() * 1000.0
    );
    std::fs::remove_dir_all(&music).unwrap();
}

// Tracks to search, in a folder made for `name`: one tagged as Picard
// tags it, with a "Work Hierarchy" tag, one without tags in a disc
// folder, a CUE sheet of two tracks, the last of which runs to the end
// of its file, and a duet with two artists; with the files, as they'd be
// opened
fn tagged_test_files(name: &str) -> (PathBuf, Vec<PathBuf>) {
    use std::borrow::Cow;

    use lofty::{
        TextEncoding,
        id3::v2::{Frame, FrameId, Id3v2Tag, KeyValueFrame, TextInformationFrame},
    };

    use crate::utils::{write_test_id3v2, write_test_wav};

    fn text(id: &'static str, value: &str) -> Frame<'static> {
        Frame::Text(TextInformationFrame::new(
            FrameId::Valid(Cow::Borrowed(id)),
            TextEncoding::UTF8,
            value.to_string(),
        ))
    }

    let music = test_dir(name);
    let album = music.join("Brandenburg Concertos");
    let disc = music.join("Untagged/CD 2");
    let sheet = music.join("Sheet");
    let compilation = music.join("Compilation");
    for dir in [&album, &disc, &sheet, &compilation] {
        std::fs::create_dir_all(dir).unwrap();
    }

    // Tagged as MusicBrainz Picard tags it, with a "Work Hierarchy" tag
    let tagged = album.join("01 Allegro.wav");
    write_test_wav(&tagged, 2);
    let mut id3v2 = Id3v2Tag::new();
    for (id, value) in [
        (
            "TIT2",
            "Brandenburg Concerto No. 2 in F major, BWV 1047: I. Allegro",
        ),
        ("TPE1", "Musica Antiqua Köln, Reinhard Goebel"),
        ("TPE2", "Johann Sebastian Bach; Musica Antiqua Köln"),
        ("TALB", "Brandenburgische Konzerte"),
        ("TCOM", "Johann Sebastian Bach"),
        ("TPE3", "Reinhard Goebel"),
        ("TCON", "Baroque"),
        ("TIT1", "Concertos"),
        ("TPOS", "1/2"),
        ("TRCK", "1/6"),
        ("TDRC", "1986"),
        ("TPUB", "Erato"),
    ] {
        id3v2.insert(text(id, value));
    }
    id3v2.insert(Frame::KeyValue(KeyValueFrame::new(
        FrameId::Valid(Cow::Borrowed("TMCL")),
        TextEncoding::UTF8,
        vec![("violin".into(), "Reinhard Goebel".into())],
    )));
    let hierarchy = serde_json::json!({ "version": 1, "segments": [{
            "work_index": 0, "attributes": ["live"], "path": [
                { "id": "1a0d0fe3-55e6-4d0e-b4b3-6d1a8e6c3f9b",
                  "title": "Brandenburg Concerto no. 2 in F major, BWV 1047",
                  "name": "Brandenburg Concerto no. 2 in F major, BWV 1047",
                  "type": "Concerto" },
                { "id": "2b1e1fe4-66f7-4e1f-c5c4-7e2b9f7d4a0c",
                  "title": "Brandenburg Concerto no. 2 in F major, BWV 1047: I. Allegro",
                  "name": "Allegro", "number": 1, "position": 1 },
            ] }] });
    let _ = id3v2.insert_user_text("WORK_HIERARCHY".to_string(), hierarchy.to_string());
    write_test_id3v2(&tagged, &id3v2, false);

    // Without tags, named by its file and its folder
    write_test_wav(&disc.join("03 - Gigue.wav"), 1);

    // A CUE sheet, whose last track runs to the end of the file
    write_test_wav(&sheet.join("Recital.wav"), 10);
    let cue = sheet.join("Recital.cue");
    std::fs::write(
        &cue,
        "PERFORMER \"Rosalyn Tureck\"\nTITLE \"Recital\"\nFILE \"Recital.wav\" WAVE\n\
             TRACK 01 AUDIO\nTITLE \"Partita no. 1, BWV 825: Praeludium\"\nINDEX 01 00:00:00\n\
             TRACK 02 AUDIO\nINDEX 01 00:04:00\n",
    )
    .unwrap();

    // A duet on a compilation, without an album artist, whose tags name
    // each of its two singers as an artist of its own
    let duet = compilation.join("07 Duet.wav");
    write_test_wav(&duet, 1);
    let mut id3v2 = Id3v2Tag::new();
    for (id, value) in [
        ("TIT2", "Duet"),
        ("TPE1", "Janet Baker\0Dietrich Fischer-Dieskau"),
        ("TALB", "Duets"),
    ] {
        id3v2.insert(text(id, value));
    }
    write_test_id3v2(&duet, &id3v2, false);

    let files = vec![
        tagged,
        disc.join("03 - Gigue.wav"),
        cue,
        sheet.join("Recital.wav"),
        duet,
    ];
    (music, files)
}

#[test]
fn the_queue_and_the_library_encode_the_same_fields() {
    use gtk::gio::prelude::FileExt;

    let (music, files) = tagged_test_files("fields");
    let [tagged, gigue, cue, _, duet] = <[PathBuf; 5]>::try_from(files.clone()).unwrap();

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    let library: std::collections::BTreeMap<PathBuf, String> = conn
        .prepare("SELECT path, fields FROM tracks")
        .unwrap()
        .query_map([], |row| Ok((path_from_bytes(row.get(0)?), row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    let files = files.iter().map(gtk::gio::File::for_path).collect();
    let receiver = crate::audio::load_tracks(files, false);
    let queue: std::collections::BTreeMap<PathBuf, String> =
        std::iter::from_fn(|| receiver.recv_blocking().ok())
            .flatten()
            .map(|data| (data.file().path().unwrap(), data.fields().to_string()))
            .collect();

    assert_eq!(library.len(), 5);
    assert_eq!(queue, library);
    // What they have in common is the point; a few lines show that
    // they're the fields a search needs
    let fields = &library[&tagged];
    for line in [
        "fm\tconcerto",
        "ca\tbwv 1047",
        "is\tlive",
        "is\tmovement",
        "ro\tviolin",
        "gr\tconcertos",
    ] {
        assert!(fields.contains(&format!("{line}\n")), "{line} in {fields}");
    }
    let untagged = &library[&gigue];
    assert!(untagged.contains("ti\tgigue\n"), "{untagged}");
    assert!(untagged.contains("al\tuntagged\n"), "{untagged}");
    let second = &library[&cue.join("track02")];
    assert!(second.contains("ln\t6\n"), "{second}");
    // Both of the artists of a compilation's track, not only the first
    let duet = &library[&duet];
    assert!(
        duet.contains("pf\tjanet baker, dietrich fischer-dieskau\n"),
        "{duet}"
    );

    std::fs::remove_dir_all(&music).unwrap();
}

// The tracks the library finds with a filter, by their paths
fn library_finds(conn: &rusqlite::Connection, filter: &super::TrackFilter) -> Vec<PathBuf> {
    let (conditions, values) = filter.conditions();
    let mut paths: Vec<PathBuf> = conn
        .prepare(&format!("SELECT path FROM tracks WHERE 1{conditions}"))
        .unwrap()
        .query_map(rusqlite::params_from_iter(values), |row| {
            Ok(path_from_bytes(row.get(0)?))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    paths.sort();
    paths
}

#[test]
fn the_queue_and_the_library_find_the_same_tracks() {
    use gtk::gio::prelude::FileExt;

    use crate::search_query::Context;

    let (music, files) = tagged_test_files("finds");
    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    let receiver =
        crate::audio::load_tracks(files.iter().map(gtk::gio::File::for_path).collect(), false);
    let tracks: Vec<crate::audio::Track> = std::iter::from_fn(|| receiver.recv_blocking().ok())
        .flatten()
        .map(crate::audio::Track::new)
        .collect();
    assert_eq!(tracks.len(), 5);

    // A favorite, played 4 times, and the other tracks never heard
    let uri = gtk::gio::File::for_path(&files[0]).uri().to_string();
    let stats = crate::play_stats::SearchStats {
        heard: [(
            uri.clone(),
            crate::play_stats::TrackStats {
                plays: 4,
                last_heard: 1_700_000_000,
            },
        )]
        .into(),
        favorites: [(uri, 1_700_000_100)].into(),
        ..Default::default()
    };
    super::TEST_SEARCH_STATS.set(stats.clone());
    let mut sources = crate::search::QueueSources::default();
    sources.stats = stats;
    let context = Context::now();

    for (text, found) in [
        ("allegro", 1),
        ("composer:bach", 1),
        ("-composer:bach", 4),
        ("composer:(bach OR handel)", 1),
        ("people:goebel", 1),
        ("year:<1990", 1),
        ("released:1986", 1),
        ("is:live", 1),
        ("-is:live", 4),
        ("has:work", 1),
        ("form:concerto", 1),
        ("catalogue:bwv825", 1),
        ("catalogue:bwv", 2),
        ("length:>5", 1),
        ("path:sheet", 2),
        ("path:untagged/cd", 1),
        ("\"rosalyn tureck\"", 2),
        ("bach OR tureck", 3),
        ("is:favorite", 1),
        ("-is:favorite", 4),
        ("favorited:2023", 1),
        ("plays:>3", 1),
        ("plays:0", 4),
        ("played:<2024", 1),
        ("has:played", 1),
        ("-has:played", 4),
        ("format:wav", 5),
        ("performer:\"fischer-dieskau\"", 1),
        ("performer:\"janet baker\"", 1),
    ] {
        let query = super::TrackFilter::query(text).unwrap();
        let filter = super::TrackFilter {
            query: Some(query.clone()),
            ..Default::default()
        };
        let library = library_finds(&conn, &filter);
        let mut queue: Vec<PathBuf> = tracks
            .iter()
            .filter(|track| track.matches_query(&query, &context, Some(&sources)))
            .map(|track| track.file().path().unwrap())
            .collect();
        queue.sort();
        assert_eq!(queue, library, "{text}");
        assert_eq!(library.len(), found, "{text}: {library:?}");
    }

    std::fs::remove_dir_all(&music).unwrap();
}

#[test]
fn a_statement_without_its_search_fails() {
    // Rather than matching with another statement's search
    let conn = test_db();
    let result: rusqlite::Result<bool> =
        conn.query_row("SELECT dismas_match(NULL, 'bach', '', NULL)", [], |row| {
            row.get(0)
        });
    assert!(result.is_err());
}

// Times searching a library of 50,000 tracks with plain words, as
// before the search syntax, with `instr()`, and through `dismas_match()`:
//   cargo test --release time_library_searches -- --ignored --nocapture
#[test]
#[ignore]
fn time_library_searches() {
    let conn = test_db();
    let tx = conn.unchecked_transaction().unwrap();
    for i in 0..50_000 {
        let composer = [
            "johann sebastian bach",
            "georg friedrich handel",
            "antonio vivaldi",
        ][i % 3];
        let search = format!(
            "{composer} cantata no. {i} for soloists, choir and orchestra recorded live {}",
            "with a long comment about the recording and the performers ".repeat(3)
        );
        let fields = format!(
            "ti\tcantata no. {i}\nco\t{composer}\nyr\t{}\nge\tbaroque\nft\tflac\nln\t{}\n",
            1960 + i % 60,
            200 + i % 300
        );
        tx.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, search, fields)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
            rusqlite::params![
                format!("/music/{i}.flac").as_bytes(),
                (i / 10).to_string(),
                search,
                fields
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let median = |run: &dyn Fn() -> usize| {
        let mut times: Vec<std::time::Duration> = (0..7)
            .map(|_| {
                let start = std::time::Instant::now();
                assert!(run() > 0);
                start.elapsed()
            })
            .collect();
        times.sort();
        times[3]
    };
    let before = median(&|| {
        conn.prepare(
            "SELECT album_id, SUM(CASE WHEN instr(search, ?) > 0 THEN 1 ELSE 0 END) AS matching,
                        COUNT(*)
                 FROM tracks WHERE album_id IS NOT NULL GROUP BY album_id HAVING matching > 0",
        )
        .unwrap()
        .query_map(["bach"], |_| Ok(()))
        .unwrap()
        .count()
    });
    let search = |text: &str| {
        let filter = super::TrackFilter {
            query: super::TrackFilter::query(text),
            ..Default::default()
        };
        median(&|| {
            super::query_search_albums(&conn, &filter, &[])
                .unwrap()
                .len()
        })
    };
    let words = search("bach");
    let fields = search("composer:bach year:<1990");
    println!(
        "instr: {:.1} ms; dismas_match, words: {:.1} ms, fields: {:.1} ms",
        before.as_secs_f64() * 1000.0,
        words.as_secs_f64() * 1000.0,
        fields.as_secs_f64() * 1000.0
    );
    assert!(words.as_secs_f64() <= before.as_secs_f64() * 1.5);
}

// Times listing the performers of a library of 50,000 tracks, as the
// Performers picker does each time it opens, tagged as Picard tags
// classical music: each track credits its conductor, and gives its two
// composers, of three names each, for the track and again for its work:
//   cargo test --release time_performers -- --ignored --nocapture
#[test]
#[ignore]
fn time_performers() {
    let conn = test_db();
    let tx = conn.unchecked_transaction().unwrap();
    for i in 0..50_000 {
        let album = i / 10;
        let conductor = format!("Conductor {}", album % 1000);
        let performers = format!(
            "\nc\t{}\t{conductor}\t{conductor}\t\n",
            conductor.to_lowercase()
        );
        let composers = format!(
            "\njohann composer {a}\tJohann Composer {a}\t\njohann composer {b}\tJohann Composer {b}\t\n",
            a = album % 700,
            b = album % 900
        );
        tx.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, performers,
                                 composers, work_composers, work_key, search)
             VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, ?4, ?5, '')",
            rusqlite::params![
                format!("/music/{i}.flac").as_bytes(),
                album.to_string(),
                performers,
                composers,
                format!("w{}", i / 4)
            ],
        )
        .unwrap();
    }
    tx.commit().unwrap();

    let all = super::TrackFilter::default();
    let mut times: Vec<std::time::Duration> = (0..7)
        .map(|_| {
            let start = std::time::Instant::now();
            let performers =
                super::query_performers(&conn, &[], &all, super::Counted::Albums).unwrap();
            assert_eq!(performers.len(), 1000);
            start.elapsed()
        })
        .collect();
    times.sort();
    println!("performers: {:.1} ms", times[3].as_secs_f64() * 1000.0);
}

#[test]
fn folders_inside_others_are_left_out() {
    use super::outermost;

    let keyed = |keys: &[&str]| -> Vec<(PathBuf, usize)> {
        keys.iter()
            .enumerate()
            .map(|(i, key)| (PathBuf::from(key), i))
            .collect()
    };
    assert_eq!(outermost(keyed(&["/m/a/sub", "/m/a"])), vec![1]);
    assert_eq!(outermost(keyed(&["/m/a", "/m/a/sub"])), vec![0]);
    assert_eq!(outermost(keyed(&["/m/a", "/m/b", "/m/a"])), vec![0, 1]);
    // Not a folder of /m/a
    assert_eq!(outermost(keyed(&["/m/a", "/m/ab"])), vec![0, 1]);
}

#[test]
fn missing_and_empty_folders_are_unavailable() {
    let dir = test_dir("roots");
    let (full, empty, missing) = (dir.join("full"), dir.join("empty"), dir.join("missing"));
    std::fs::create_dir_all(&full).unwrap();
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(full.join("file"), "").unwrap();

    let roots = roots_of(&[&full, &empty, &missing]);
    assert_eq!(roots.available, vec![full]);
    assert_eq!(roots.unavailable, vec![empty, missing]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_settings_list_the_folders() {
    use gtk::gio::prelude::*;

    use super::{default_music_dir, folders_from_entries};

    assert_eq!(
        folders_from_entries(Vec::new(), Vec::new()),
        vec![MusicFolder::new(default_music_dir())]
    );
    assert_eq!(
        folders_from_entries(
            vec![
                ("file:///music/A".to_string(), String::new()),
                (
                    "file:///run/user/1000/doc/1a2b/B".to_string(),
                    "file:///media/drive/B".to_string()
                ),
            ],
            vec![
                "file:///run/user/1000/doc/1a2b/B/Podcasts/Old".to_string(),
                "file:///music/A/Audiobooks".to_string(),
                "file:///music/A/Audiobooks".to_string(),
                // Neither in a folder, nor one of its folders
                "file:///music/Other".to_string(),
                "file:///music/A".to_string(),
            ]
        ),
        vec![
            MusicFolder {
                path: PathBuf::from("/music/A"),
                host: None,
                excluded: vec![PathBuf::from("Audiobooks")],
            },
            MusicFolder {
                path: PathBuf::from("/run/user/1000/doc/1a2b/B"),
                host: Some(PathBuf::from("/media/drive/B")),
                excluded: vec![PathBuf::from("Podcasts/Old")],
            },
        ]
    );

    // Those of the XDG Music folder too
    let audiobooks = default_music_dir().join("Audiobooks");
    let uri = gtk::gio::File::for_path(&audiobooks).uri().to_string();
    assert_eq!(
        folders_from_entries(Vec::new(), vec![uri])[0].excluded,
        vec![PathBuf::from("Audiobooks")]
    );
}

#[test]
fn left_out_folders_are_not_read() {
    use crate::utils::write_test_wav;

    let dir = test_dir("excluded");
    for folder in ["Album", "Audiobooks/Book", "Podcasts/New", "Podcasts/Old"] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    write_test_wav(&dir.join("Album/One.wav"), 1);
    write_test_wav(&dir.join("Audiobooks/Book/Two.wav"), 1);
    write_test_wav(&dir.join("Podcasts/New/Three.wav"), 1);
    write_test_wav(&dir.join("Podcasts/Old/Four.wav"), 1);

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let paths = |conn: &rusqlite::Connection| -> Vec<String> {
        tracks(conn, &dir).into_iter().map(|row| row.path).collect()
    };
    let folder = |excluded: &[&str]| MusicFolder {
        excluded: excluded.iter().map(PathBuf::from).collect(),
        ..MusicFolder::new(dir.clone())
    };

    let roots = super::roots(&[folder(&["Podcasts/Old"])]);
    assert_eq!(roots.excluded, vec![dir.join("Podcasts/Old")]);
    scan_roots(&mut conn, &roots, &sender).unwrap();
    assert_eq!(
        paths(&conn),
        vec![
            "Album/One.wav",
            "Audiobooks/Book/Two.wav",
            "Podcasts/New/Three.wav"
        ]
    );

    // The tracks read before a folder was left out are forgotten
    let roots = super::roots(&[folder(&["Audiobooks", "Podcasts"])]);
    let (result, _) = scan_roots(&mut conn, &roots, &sender).unwrap();
    assert_eq!(result.n_changed, 2);
    assert_eq!(paths(&conn), vec!["Album/One.wav"]);

    // And read again once it's back
    let (result, _) = scan_roots(&mut conn, &super::roots(&[folder(&[])]), &sender).unwrap();
    assert_eq!(result.n_changed, 3);

    // Those of a folder inside another one don't count, as the other
    // one is read
    let inner = MusicFolder {
        excluded: vec![PathBuf::from("Old")],
        ..MusicFolder::new(dir.join("Podcasts"))
    };
    let roots = super::roots(&[folder(&[]), inner]);
    assert!(roots.excluded.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn left_out_folders_are_relative_to_their_music_folder() {
    use super::relative_to;

    let dir = test_dir("relative");
    let music = dir.join("Music");
    std::fs::create_dir_all(music.join("Audiobooks")).unwrap();
    let link = dir.join("Link");
    std::os::unix::fs::symlink(&music, &link).unwrap();

    let folder = |path: PathBuf| MusicFolder::new(path);
    assert_eq!(
        relative_to(&folder(music.join("Audiobooks")), &folder(music.clone())),
        Some(PathBuf::from("Audiobooks"))
    );
    // Picked through a link to the music folder
    assert_eq!(
        relative_to(&folder(link.join("Audiobooks")), &folder(music.clone())),
        Some(PathBuf::from("Audiobooks"))
    );
    assert_eq!(
        relative_to(&folder(music.clone()), &folder(music.clone())),
        None
    );
    assert_eq!(relative_to(&folder(dir.clone()), &folder(music)), None);
    // Through the document portal, by the paths outside the sandbox
    let portal = |path: &str, host: &str| MusicFolder {
        host: Some(PathBuf::from(host)),
        ..MusicFolder::new(PathBuf::from(path))
    };
    assert_eq!(
        relative_to(
            &portal("/run/user/1000/doc/9z/Old", "/media/drive/B/Podcasts/Old"),
            &portal("/run/user/1000/doc/1a2b/B", "/media/drive/B")
        ),
        Some(PathBuf::from("Podcasts/Old"))
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tracks_of_several_folders() {
    use crate::utils::write_test_wav;

    let dir = test_dir("folders");
    let (a, b) = (dir.join("A"), dir.join("B"));
    let sub = a.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    write_test_wav(&a.join("One.wav"), 1);
    write_test_wav(&sub.join("Two.wav"), 1);
    write_test_wav(&b.join("Three.wav"), 1);

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let paths = |conn: &rusqlite::Connection| -> Vec<String> {
        tracks(conn, &dir).into_iter().map(|row| row.path).collect()
    };

    let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
    assert_eq!(result.n_changed, 3);
    assert_eq!(
        paths(&conn),
        vec!["A/One.wav", "A/sub/Two.wav", "B/Three.wav"]
    );

    // A folder inside another one is read once, either way round
    for roots in [roots_of(&[&a, &sub, &b]), roots_of(&[&sub, &a, &b])] {
        let (result, _) = scan_roots(&mut conn, &roots, &sender).unwrap();
        assert_eq!(result.n_changed, 0);
        assert_eq!(paths(&conn).len(), 3);
    }

    // The tracks of a folder that isn't there are kept, as are those of
    // an empty one, which is often a drive that isn't mounted
    let away = dir.join("B-away");
    std::fs::rename(&b, &away).unwrap();
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
    assert_eq!(result.n_changed, 0);
    assert_eq!(paths(&conn).len(), 3);
    std::fs::rename(&away, &b).unwrap();
    std::fs::rename(b.join("Three.wav"), dir.join("Three.wav")).unwrap();
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
    assert_eq!(result.n_changed, 0);
    assert_eq!(paths(&conn).len(), 3);

    // Those of a folder that was removed from the list go
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&a]), &sender).unwrap();
    assert_eq!(result.n_changed, 1);
    assert_eq!(paths(&conn), vec!["A/One.wav", "A/sub/Two.wav"]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn a_folder_linked_to_another_is_read_once() {
    use crate::utils::write_test_wav;

    let dir = test_dir("link");
    let a = dir.join("A");
    std::fs::create_dir_all(&a).unwrap();
    write_test_wav(&a.join("One.wav"), 1);
    let b = dir.join("B");
    std::os::unix::fs::symlink(&a, &b).unwrap();

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    let (result, _) = scan_roots(&mut conn, &roots_of(&[&a, &b]), &sender).unwrap();
    assert_eq!(result.n_changed, 1);
    let found: Vec<String> = tracks(&conn, &dir)
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(found, vec!["A/One.wav"]);

    std::fs::remove_dir_all(&dir).unwrap();
}

// As in a library that beets makes of links, or with a folder on
// another drive linked into the music folder
#[cfg(unix)]
#[test]
fn symbolic_links_in_a_music_folder_are_followed_once() {
    use std::os::unix::fs::symlink;

    use crate::utils::write_test_wav;

    let music = test_dir("links");
    std::fs::create_dir_all(music.join("Real")).unwrap();
    write_test_wav(&music.join("Real/One.wav"), 1);
    let elsewhere = test_dir("links-elsewhere");
    write_test_wav(&elsewhere.join("Two.wav"), 1);
    symlink(&elsewhere, music.join("Linked")).unwrap();
    let other = test_dir("links-other");
    write_test_wav(&other.join("Three.wav"), 1);
    symlink(other.join("Three.wav"), music.join("Single.wav")).unwrap();
    // Links to what the folder has already, and a loop, add nothing
    symlink(music.join("Real"), music.join("Again")).unwrap();
    symlink(music.join("Real/One.wav"), music.join("Real/Copy.wav")).unwrap();
    symlink(&music, music.join("Real/Loop")).unwrap();
    // Nor does a link to nothing
    symlink(music.join("Nothing"), music.join("Gone")).unwrap();

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    scan_roots(&mut conn, &roots_of(&[&music]), &sender).unwrap();
    let found: Vec<String> = tracks(&conn, &music)
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(found, ["Linked/Two.wav", "Real/One.wav", "Single.wav"]);

    std::fs::remove_dir_all(&music).unwrap();
    std::fs::remove_dir_all(&elsewhere).unwrap();
    std::fs::remove_dir_all(&other).unwrap();
}

#[test]
fn trash_and_hidden_folders_are_not_read() {
    use crate::utils::write_test_wav;

    let dir = test_dir("hidden");
    for folder in [
        "Album",
        ".Trash-1000/files",
        "$Recycle.Bin",
        "#recycle",
        "#snapshot/daily",
        "Album/@eaDir",
    ] {
        std::fs::create_dir_all(dir.join(folder)).unwrap();
    }
    write_test_wav(&dir.join("Album/One.wav"), 1);
    write_test_wav(&dir.join("Album/.Two.wav"), 1);
    write_test_wav(&dir.join(".Trash-1000/files/Three.wav"), 1);
    write_test_wav(&dir.join("$Recycle.Bin/$R4ND0M.wav"), 1);
    write_test_wav(&dir.join("#recycle/Four.wav"), 1);
    write_test_wav(&dir.join("#snapshot/daily/One.wav"), 1);
    write_test_wav(&dir.join("Album/@eaDir/One.wav"), 1);

    let mut conn = test_db();
    let (sender, _receiver) = async_channel::unbounded();
    scan_roots(&mut conn, &roots_of(&[&dir]), &sender).unwrap();
    let found: Vec<String> = tracks(&conn, &dir)
        .into_iter()
        .map(|row| row.path)
        .collect();
    assert_eq!(found, vec!["Album/One.wav"]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_albums_of_hidden_folders_are_left_out() {
    use super::query_albums;

    let conn = test_db();
    for (path, album) in [
        ("/m/A/One.flac", "a"),
        ("/m/B/Two.flac", "b"),
        ("/m/B/sub/Three.flac", "b"),
        ("/m/Bb/Four.flac", "c"),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2)",
            rusqlite::params![path.as_bytes(), album],
        )
        .unwrap();
    }
    let ids = |hidden: &[PathBuf]| -> Vec<String> {
        let mut ids: Vec<String> = query_albums(&conn, hidden)
            .unwrap()
            .into_iter()
            .map(|album| album.id)
            .collect();
        ids.sort();
        ids
    };

    assert_eq!(ids(&[]), vec!["a", "b", "c"]);
    assert_eq!(ids(&[PathBuf::from("/m/B")]), vec!["a", "c"]);
    assert_eq!(
        ids(&[PathBuf::from("/m/A"), PathBuf::from("/m/Bb/")]),
        vec!["b"]
    );

    // An album's tracks, and the album of a track, leave them out too
    let all = super::TrackFilter::default();
    let sub = [PathBuf::from("/m/B/sub")];
    let three = Path::new("/m/B/sub/Three.flac");
    let album_tracks = |hidden: &[PathBuf]| {
        super::query_album_tracks(&conn, "b", &all, hidden)
            .unwrap()
            .into_iter()
            .map(|(_, path)| path)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        album_tracks(&[]),
        [PathBuf::from("/m/B/Two.flac"), three.to_path_buf()]
    );
    assert_eq!(album_tracks(&sub), [PathBuf::from("/m/B/Two.flac")]);
    assert_eq!(
        super::query_track_album(&conn, three, &[])
            .unwrap()
            .as_deref(),
        Some("b")
    );
    assert_eq!(super::query_track_album(&conn, three, &sub).unwrap(), None);
}

#[test]
fn the_folders_the_scan_reads_are_watched() {
    let music = test_dir("watch");
    for folder in [
        "Bach/Partitas/CD1",
        "Bach/Partitas/CD2",
        "Audiobooks/A",
        ".Trash-1000/x",
    ] {
        std::fs::create_dir_all(music.join(folder)).unwrap();
    }
    std::fs::write(music.join("Bach/Partitas/CD1/01.opus"), b"").unwrap();
    // A link to a folder elsewhere is watched as the scan reads it, and
    // one back into the music folder adds nothing
    let elsewhere = test_dir("watch-elsewhere");
    std::fs::create_dir_all(elsewhere.join("Jazz")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, music.join("Linked")).unwrap();
    std::os::unix::fs::symlink(&music, music.join("Bach/Partitas/Back")).unwrap();

    let mut folder = MusicFolder::new(music.clone());
    folder.excluded.push(PathBuf::from("Audiobooks"));
    let folders = [folder];
    let list = watch_list(&folders);
    let mut watched = list.folders;
    watched.sort();
    let relative: Vec<&Path> = watched
        .iter()
        .map(|path| path.strip_prefix(&music).unwrap())
        .collect();
    assert_eq!(
        relative,
        [
            Path::new(""),
            Path::new("Bach"),
            Path::new("Bach/Partitas"),
            Path::new("Bach/Partitas/CD1"),
            Path::new("Bach/Partitas/CD2"),
            Path::new("Linked"),
            Path::new("Linked/Jazz"),
        ]
    );
    assert!(!list.unwatched);
    assert_eq!(
        list.available,
        [
            music.canonicalize().unwrap(),
            elsewhere.canonicalize().unwrap()
        ]
    );
    assert!(!list.unavailable);

    // A link that leads nowhere can be to a drive that isn't mounted
    let gone = music.join("Gone");
    std::os::unix::fs::symlink(music.join("Nothing"), &gone).unwrap();
    let list = watch_list(&folders);
    assert!(list.unavailable);
    std::fs::remove_file(gone).unwrap();

    // A folder that came through the document portal isn't watched
    let folders = [MusicFolder {
        host: Some(PathBuf::from("/media/music")),
        ..MusicFolder::new(music.clone())
    }];
    let list = watch_list(&folders);
    assert!(list.folders.is_empty());
    assert!(list.unwatched);

    // A drive mounted anywhere can bring back a folder that isn't there
    let folders = [MusicFolder::new(music.join("Unplugged"))];
    let list = watch_list(&folders);
    assert!(list.folders.is_empty());
    assert!(list.unavailable);

    std::fs::remove_dir_all(music).unwrap();
    std::fs::remove_dir_all(elsewhere).unwrap();
}

#[test]
fn the_tracks_of_each_album_are_in_their_order() {
    let conn = test_db();
    for (path, album, disc, track) in [
        ("/m/B/2-01.flac", "b", 2, 1),
        ("/m/A/02.flac", "a", 1, 2),
        ("/m/B/1-01.flac", "b", 1, 1),
        ("/m/A/01.flac", "a", 1, 1),
        ("/m/Gone/01.flac", "c", 1, 1),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, disc, track)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
            rusqlite::params![path.as_bytes(), album, disc, track],
        )
        .unwrap();
    }
    let albums = super::query_album_track_paths(&conn, &[PathBuf::from("/m/Gone")]).unwrap();
    let paths = |paths: &[&str]| paths.iter().map(PathBuf::from).collect::<Vec<_>>();
    assert_eq!(
        albums,
        [
            ("a".to_string(), paths(&["/m/A/01.flac", "/m/A/02.flac"])),
            (
                "b".to_string(),
                paths(&["/m/B/1-01.flac", "/m/B/2-01.flac"])
            ),
        ]
    );
}

#[test]
fn albums_are_on_several_discs_by_number_or_folder() {
    use super::Disc;

    let conn = test_db();
    for (path, album, disc) in [
        // Disc numbers, the folder aside
        ("/m/Box/1-01.flac", "box", Some(1)),
        ("/m/Box/2-01.flac", "box", Some(2)),
        // One disc, numbered or not
        ("/m/One/01.flac", "one", Some(1)),
        ("/m/One/02.flac", "one", None),
        // Disc folders without numbers
        ("/m/Opera/CD 1/01.flac", "opera", None),
        ("/m/Opera/CD 2/01.flac", "opera", None),
        // A disc that's hidden
        ("/m/Hidden/CD 1/01.flac", "hidden", None),
        ("/m/Gone/CD 2/01.flac", "hidden", None),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, disc)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
            rusqlite::params![path.as_bytes(), album, disc],
        )
        .unwrap();
    }
    let gone = [PathBuf::from("/m/Gone")];
    let all = super::TrackFilter::default();
    let has_discs = |album| {
        super::on_several_discs(&super::query_album_tracks(&conn, album, &all, &gone).unwrap())
    };
    assert!(has_discs("box"));
    assert!(!has_discs("one"));
    assert!(has_discs("opera"));
    assert!(!has_discs("hidden"));
    assert!(!has_discs("missing"));

    let folder = Path::new("/m/Opera/Disc 2 - Act III");
    assert_eq!(Disc::of(Some(3), Some(folder)), Disc::Number(3));
    assert_eq!(
        Disc::of(None, Some(folder)),
        Disc::Folder(folder.to_path_buf())
    );
    assert_eq!(Disc::of(None, Some(Path::new("/m/Opera"))), Disc::Number(1));
    assert_eq!(Disc::of(None, None), Disc::Number(1));
}

#[test]
fn tracks_without_numbers_are_in_the_order_of_their_names() {
    let conn = test_db();
    let add = |path: &str, album: &str, track: Option<i64>| {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, track)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
            rusqlite::params![path.as_bytes(), album, track],
        )
        .unwrap();
    };
    // Numbers in their order; case goes by the locale, which is C in
    // the tests
    for name in ["10 Ten", "b", "2 Two", "1 One", "a"] {
        add(&format!("/m/Untagged/{name}.flac"), "untagged", None);
    }
    // Discs in folders of their own, without disc numbers
    for (path, track) in [
        ("/m/Set/CD2/01.flac", 1),
        ("/m/Set/CD1/02.flac", 2),
        ("/m/Set/CD2/02.flac", 2),
        ("/m/Set/CD1/01.flac", 1),
    ] {
        add(path, "set", Some(track));
    }

    let names = |album: &str| -> Vec<String> {
        super::query_album_track_paths(&conn, &[])
            .unwrap()
            .into_iter()
            .find(|(id, _)| id == album)
            .unwrap()
            .1
            .iter()
            .map(|path| path.strip_prefix("/m").unwrap().display().to_string())
            .collect()
    };
    assert_eq!(
        names("untagged"),
        [
            "Untagged/1 One.flac",
            "Untagged/2 Two.flac",
            "Untagged/10 Ten.flac",
            "Untagged/a.flac",
            "Untagged/b.flac",
        ]
    );
    assert_eq!(
        names("set"),
        [
            "Set/CD1/01.flac",
            "Set/CD1/02.flac",
            "Set/CD2/01.flac",
            "Set/CD2/02.flac",
        ]
    );
}

#[test]
fn composers_without_a_sort_name_are_sorted_by_their_last_name() {
    use super::sort_name;
    assert_eq!(sort_name("Johann Sebastian Bach"), "Bach, Johann Sebastian");
    assert_eq!(sort_name("Ludwig van Beethoven"), "Beethoven, Ludwig van");
    assert_eq!(sort_name("Johann Strauss II"), "Strauss, Johann II");
    assert_eq!(sort_name("Pérotin"), "Pérotin");
    assert_eq!(
        sort_name("Bach, Johann Sebastian"),
        "Bach, Johann Sebastian"
    );
}

#[test]
fn the_composers_of_a_track_keep_their_sort_names() {
    use super::composer_entries;
    let names = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(composer_entries(&[], &[]), None);
    assert_eq!(
        composer_entries(
            &names(&["Antonín Dvořák", "Josef Suk"]),
            &names(&["Dvořák, Antonín", "Suk, Josef"])
        )
        .as_deref(),
        Some(
            "\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\njosef suk\tJosef Suk\tSuk, Josef\n"
        )
    );
    // Without as many sort names, they're left out, to be worked out
    assert_eq!(
        composer_entries(
            &names(&["Antonín Dvořák", "Josef Suk"]),
            &names(&["Suk, Josef"])
        )
        .as_deref(),
        Some("\nantonin dvorak\tAntonín Dvořák\t\njosef suk\tJosef Suk\t\n")
    );
}

#[test]
fn the_composers_of_a_work_take_the_sort_names_of_their_tags() {
    use super::credited_work_composers;
    use crate::audio::TrackCredits;
    let names = |names: &[&str]| names.iter().map(ToString::to_string).collect::<Vec<_>>();
    let sorts = |credits: &TrackCredits, artist| {
        credited_work_composers(credits, artist)
            .into_iter()
            .map(|composer| (composer.key, composer.sort))
            .collect::<Vec<_>>()
    };

    // The work's composers, matched to the track's sort names by name
    let requiem = TrackCredits {
        composers: names(&["Franz Xaver Süßmayr", "Wolfgang Amadeus Mozart"]),
        composer_sorts: names(&["Süßmayr, Franz Xaver", "Mozart, W. A."]),
        work_composers: names(&["Wolfgang Amadeus Mozart"]),
        ..Default::default()
    };
    assert_eq!(
        sorts(&requiem, None),
        [(
            "wolfgang amadeus mozart".to_string(),
            "Mozart, W. A.".to_string()
        )]
    );

    // Without as many sort names as names, they're worked out
    let suk = TrackCredits {
        composers: names(&["Antonín Dvořák", "Josef Suk"]),
        composer_sorts: names(&["Suk, Josef"]),
        ..Default::default()
    };
    assert_eq!(
        sorts(&suk, None),
        [
            ("antonin dvorak".to_string(), "Dvořák, Antonín".to_string()),
            ("josef suk".to_string(), "Suk, Josef".to_string())
        ]
    );

    // Without composers, the artist, if one is given
    let untagged = TrackCredits::default();
    assert_eq!(
        sorts(&untagged, Some("Arcangelo Corelli")),
        [(
            "arcangelo corelli".to_string(),
            "Corelli, Arcangelo".to_string()
        )]
    );
    assert!(sorts(&untagged, None).is_empty());
}

#[test]
fn composers_without_a_sort_tag_are_listed_by_their_name() {
    let conn = test_db();
    for (path, album, composers) in [
        ("/m/A/01.flac", "a", "\npink floyd\tPink Floyd\t\n"),
        ("/m/B/01.flac", "b", "\ntan dun\tTan Dun\t\n"),
        (
            "/m/C/01.flac",
            "c",
            "\nbela bartok\tBéla Bartók\tBartók, Béla\n",
        ),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
            rusqlite::params![path.as_bytes(), album, composers],
        )
        .unwrap();
    }
    // Sorted by the last name all the same, but a sort name worked out
    // isn't shown, as it would turn a band's name around; one from the
    // tags is
    let listed: Vec<(String, String)> = super::query_composers(
        &conn,
        &[],
        &super::TrackFilter::default(),
        super::Counted::Albums,
    )
    .unwrap()
    .into_iter()
    .map(|composer| (composer.sort, composer.label))
    .collect();
    assert_eq!(
        listed,
        [
            ("Bartók, Béla".to_string(), "Bartók, Béla".to_string()),
            ("Dun, Tan".to_string(), "Tan Dun".to_string()),
            ("Floyd, Pink".to_string(), "Pink Floyd".to_string()),
        ]
    );
}

#[test]
fn albums_in_the_folders_that_are_gone_are_found() {
    let conn = test_db();
    for (path, album) in [("/m/Gone/01.opus", Some("a")), ("/m/Loose/01.opus", None)] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2)",
            rusqlite::params![path.as_bytes(), album],
        )
        .unwrap();
    }
    assert!(super::query_tracks_have(&conn, "album_id", &[]).unwrap());
    // A track in no album, which the scan stores for a file it couldn't
    // read, makes no album
    let gone = [PathBuf::from("/m/Gone")];
    assert!(!super::query_tracks_have(&conn, "album_id", &gone).unwrap());
}

#[test]
fn composers_are_listed_with_their_albums_and_found_by_their_key() {
    let conn = test_db();
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    let cpe =
        "\ncarl philipp emanuel bach\tCarl Philipp Emanuel Bach\tBach, Carl Philipp Emanuel\n";
    let both = format!("{bach}{}", &cpe[1..]);
    for (path, album, composers, search) in [
        ("/m/A/01.opus", "a", Some(bach), "bwv 1001"),
        ("/m/A/02.opus", "a", Some(bach), "bwv 1002"),
        ("/m/B/01.opus", "b", Some(cpe), "wq 182"),
        ("/m/B/02.opus", "b", Some(both.as_str()), "bwv 1037"),
        ("/m/C/01.opus", "c", None, "anon"),
        ("/m/Gone/01.opus", "d", Some(bach), "bwv 988"),
    ] {
        conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, composers, search],
            )
            .unwrap();
    }
    let hidden = [PathBuf::from("/m/Gone")];
    assert!(super::query_tracks_have(&conn, "composers", &hidden).unwrap());
    let only_anonymous = [
        PathBuf::from("/m/A"),
        PathBuf::from("/m/B"),
        PathBuf::from("/m/Gone"),
    ];
    assert!(!super::query_tracks_have(&conn, "composers", &only_anonymous).unwrap());

    let all = super::TrackFilter::default();
    let composers = super::query_composers(&conn, &hidden, &all, super::Counted::Albums).unwrap();
    let listed: Vec<(&str, u32)> = composers
        .iter()
        .map(|composer| (composer.sort.as_str(), composer.count))
        .collect();
    assert_eq!(
        listed,
        [
            ("Bach, Carl Philipp Emanuel", 1),
            ("Bach, Johann Sebastian", 2)
        ]
    );
    // The list's search looks into both names
    assert_eq!(
        composers[1].search,
        "johann sebastian bach bach, johann sebastian"
    );

    // With a filter, only the composers of the tracks it matches, with
    // the albums of those tracks
    let listed = |words: &[&str]| -> Vec<(String, u32)> {
        let filter = super::TrackFilter {
            query: super::TrackFilter::query(&words.join(" ")),
            ..Default::default()
        };
        super::query_composers(&conn, &hidden, &filter, super::Counted::Albums)
            .unwrap()
            .into_iter()
            .map(|composer| (composer.sort, composer.count))
            .collect()
    };
    assert_eq!(
        listed(&["1037"]),
        [
            ("Bach, Carl Philipp Emanuel".to_string(), 1),
            ("Bach, Johann Sebastian".to_string(), 1)
        ]
    );
    assert_eq!(
        listed(&["wq"]),
        [("Bach, Carl Philipp Emanuel".to_string(), 1)]
    );
    assert!(listed(&["mozart"]).is_empty());

    // Only Johann Sebastian's, not his son's, though the names share a
    // word, with the tracks that are his
    let filter = super::TrackFilter {
        composer: Some("johann sebastian bach".to_string()),
        ..Default::default()
    };
    let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(&conn, &filter, &hidden)
        .unwrap()
        .into_iter()
        .collect();
    albums.sort();
    assert_eq!(
        albums,
        [("a".to_string(), (2, 2)), ("b".to_string(), (1, 2))]
    );

    // and with the words of a search too
    let filter = super::TrackFilter {
        query: super::TrackFilter::query("1002"),
        ..filter
    };
    let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
    assert_eq!(albums.get("a"), Some(&(1, 2)));
    assert_eq!(albums.len(), 1);
}

#[test]
fn the_favorite_albums_are_found_with_the_other_filters() {
    let conn = test_db();
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    for (path, album, search) in [
        ("/m/A/01.opus", "a", "bwv 1001"),
        ("/m/A/02.opus", "a", "bwv 1002"),
        ("/m/B/01.opus", "b", "bwv 1003"),
        ("/m/C/01.opus", "c", "bwv 1004"),
    ] {
        conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
                rusqlite::params![path.as_bytes(), album, bach, search],
            )
            .unwrap();
    }
    let favorites = |ids: &[&str]| {
        Some(super::Favorites::Albums(
            ids.iter().map(ToString::to_string).collect(),
        ))
    };
    let albums = |filter: &super::TrackFilter| {
        let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(&conn, filter, &[])
            .unwrap()
            .into_iter()
            .collect();
        albums.sort();
        albums
    };

    let filter = super::TrackFilter {
        favorites: favorites(&["a", "c"]),
        ..Default::default()
    };
    assert!(!filter.is_empty());
    assert_eq!(
        albums(&filter),
        [("a".to_string(), (2, 2)), ("c".to_string(), (1, 1))]
    );
    // With the words of a search, and the composer's list follows
    let filter = super::TrackFilter {
        query: super::TrackFilter::query("1002"),
        ..filter
    };
    assert_eq!(albums(&filter), [("a".to_string(), (1, 2))]);
    let composers = super::query_composers(&conn, &[], &filter, super::Counted::Albums).unwrap();
    assert_eq!(composers.len(), 1);
    assert_eq!(composers[0].count, 1);

    // Without favorites, nothing
    let filter = super::TrackFilter {
        favorites: favorites(&[]),
        ..Default::default()
    };
    assert!(albums(&filter).is_empty());
}

#[test]
fn the_favorite_tracks_are_found_with_the_other_filters() {
    let conn = test_db();
    for (path, album, search) in [
        ("/m/A/01.opus", "a", "bwv 1001"),
        ("/m/A/02.opus", "a", "bwv 1002"),
        ("/m/B/01.opus", "b", "bwv 1003"),
        ("/m/C/01.opus", "c", "bwv 1004"),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3)",
            rusqlite::params![path.as_bytes(), album, search],
        )
        .unwrap();
    }
    let favorites = |paths: &[&str]| {
        Some(super::Favorites::Tracks(
            paths.iter().map(PathBuf::from).collect(),
        ))
    };
    let albums = |filter: &super::TrackFilter| {
        let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(&conn, filter, &[])
            .unwrap()
            .into_iter()
            .collect();
        albums.sort();
        albums
    };

    // One of the two tracks of an album, and the only one of another
    let filter = super::TrackFilter {
        favorites: favorites(&["/m/A/02.opus", "/m/C/01.opus"]),
        ..Default::default()
    };
    assert_eq!(
        albums(&filter),
        [("a".to_string(), (1, 2)), ("c".to_string(), (1, 1))]
    );
    // and only that track is taken from the album
    let tracks = super::query_album_tracks(&conn, "a", &filter, &[]).unwrap();
    assert_eq!(tracks, [(None, PathBuf::from("/m/A/02.opus"))]);

    // With the words of a search, only the favorites they match
    let words = |words: &[&str]| super::TrackFilter {
        query: super::TrackFilter::query(&words.join(" ")),
        ..filter.clone()
    };
    assert_eq!(albums(&words(&["1004"])), [("c".to_string(), (1, 1))]);
    assert!(albums(&words(&["1001"])).is_empty());

    // Without favorites, nothing
    let filter = super::TrackFilter {
        favorites: favorites(&[]),
        ..Default::default()
    };
    assert!(albums(&filter).is_empty());
}

#[test]
fn a_random_track_is_one_the_filter_matches() {
    let conn = test_db();
    for (path, album, title, search) in [
        ("/m/A/01.opus", Some("a"), Some("Sonata"), "bwv 1001"),
        ("/m/A/02.opus", Some("a"), None, "bwv 1002"),
        ("/m/B/01.opus", Some("b"), Some("Partita"), "bwv 1004"),
        ("/m/Gone/01.opus", Some("g"), Some("Gone"), "bwv 1003"),
        // A file whose tags couldn't be read is in no album
        ("/m/X/01.opus", None, None, "bwv 1005"),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, title, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
            rusqlite::params![path.as_bytes(), album, title, search],
        )
        .unwrap();
    }
    let hidden = [PathBuf::from("/m/Gone")];
    let tracks = |filter: &super::TrackFilter| {
        let mut tracks = super::query_matching_tracks(&conn, filter, &hidden)
            .unwrap()
            .into_iter()
            .map(|(path, title)| (path.to_string_lossy().into_owned(), title))
            .collect::<Vec<_>>();
        tracks.sort();
        tracks
    };
    let track =
        |path: &str, title: Option<&str>| (path.to_string(), title.map(ToString::to_string));

    assert_eq!(
        tracks(&super::TrackFilter::default()),
        [
            track("/m/A/01.opus", Some("Sonata")),
            track("/m/A/02.opus", None),
            track("/m/B/01.opus", Some("Partita")),
        ]
    );
    // The favorites, alone and with the words of a search
    let favorites = super::TrackFilter {
        favorites: Some(super::Favorites::Tracks(
            ["/m/A/02.opus", "/m/B/01.opus"]
                .iter()
                .map(PathBuf::from)
                .collect(),
        )),
        ..Default::default()
    };
    assert_eq!(
        tracks(&favorites),
        [
            track("/m/A/02.opus", None),
            track("/m/B/01.opus", Some("Partita")),
        ]
    );
    let words = super::TrackFilter {
        query: super::TrackFilter::query("1004"),
        ..favorites
    };
    assert_eq!(tracks(&words), [track("/m/B/01.opus", Some("Partita"))]);
}

#[test]
fn the_works_page_lists_only_the_people_of_works() {
    use super::Counted::{Albums, Works};

    let conn = test_db();
    let vivaldi = "\nantonio vivaldi\tAntonio Vivaldi\tVivaldi, Antonio\n";
    let violinist = "\ns\tenrico onofri\tEnrico Onofri\tOnofri, Enrico\tviolin\n";
    let conductor = "\nc\tgiovanni antonini\tGiovanni Antonini\tAntonini, Giovanni\t\n";
    // A sonata tagged with its work, and a concerto whose tracks only
    // name it in their titles
    for (path, album, work, performers) in [
        ("/m/A/01.opus", "a", Some("t:sonata"), violinist),
        ("/m/A/02.opus", "a", Some("t:sonata"), violinist),
        ("/m/B/01.opus", "b", None, conductor),
        ("/m/B/02.opus", "b", None, conductor),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, composers,
                                     performers, work_key, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, ?5, '')",
            rusqlite::params![path.as_bytes(), album, vivaldi, performers, work],
        )
        .unwrap();
    }

    let performers = |counted| -> Vec<(String, u32)> {
        let filter = super::TrackFilter {
            composer: Some("antonio vivaldi".to_string()),
            ..Default::default()
        };
        super::query_performers(&conn, &[], &filter, counted)
            .unwrap()
            .into_iter()
            .map(|performer| (performer.sort, performer.count))
            .collect()
    };
    assert_eq!(
        performers(Albums),
        [
            ("Antonini, Giovanni".to_string(), 1),
            ("Onofri, Enrico".to_string(), 1)
        ]
    );
    // The conductor of the concerto has no works to show
    assert_eq!(performers(Works), [("Onofri, Enrico".to_string(), 1)]);

    let composers = |performer: &str, counted| -> Vec<u32> {
        let filter = super::TrackFilter {
            performer: Some(performer.to_string()),
            ..Default::default()
        };
        super::query_composers(&conn, &[], &filter, counted)
            .unwrap()
            .into_iter()
            .map(|composer| composer.count)
            .collect()
    };
    assert_eq!(composers("giovanni antonini", Albums), [1]);
    assert!(composers("giovanni antonini", Works).is_empty());
    assert_eq!(composers("enrico onofri", Works), [1]);
}

#[test]
fn performers_are_listed_once_by_what_they_do_the_most() {
    use crate::audio::PerformerKind;

    let conn = test_db();
    let herreweghe = "c\tphilippe herreweghe\tPhilippe Herreweghe\tHerreweghe, Philippe\t";
    let cvg = "e\tcollegium vocale gent\tCollegium Vocale Gent\tCollegium Vocale Gent\t\
                   choir vocals";
    let eerens = "s\tilse eerens\tIlse Eerens\tEerens, Ilse\tsoprano vocals";
    // Named in the album artist of a track she doesn't sing on
    let eerens_guessed = "e\tilse eerens\tIlse Eerens\tIlse Eerens\t";
    let chandler = "c\tadrian chandler\tAdrian Chandler\tChandler, Adrian\t";
    let chandler_solo = "s\tadrian chandler\tAdrian Chandler\tChandler, Adrian\tsolo violin";
    let entries = |lines: &[&str]| format!("\n{}\n", lines.join("\n"));
    for (path, album, performers, composers) in [
        (
            "/m/A/01.opus",
            "a",
            entries(&[herreweghe, cvg, eerens]),
            Some("\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n"),
        ),
        (
            "/m/A/02.opus",
            "a",
            entries(&[herreweghe, eerens_guessed]),
            None,
        ),
        ("/m/B/01.opus", "b", entries(&[herreweghe, cvg]), None),
        (
            "/m/C/01.opus",
            "c",
            entries(&[chandler, chandler_solo]),
            None,
        ),
        ("/m/D/01.opus", "d", entries(&[chandler]), None),
        ("/m/D/02.opus", "d", entries(&[chandler_solo]), None),
        ("/m/Gone/01.opus", "g", entries(&[eerens]), None),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, performers,
                                     composers, search)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, '')",
            rusqlite::params![path.as_bytes(), album, performers, composers],
        )
        .unwrap();
    }
    let hidden = [PathBuf::from("/m/Gone")];
    assert!(super::query_tracks_have(&conn, "performers", &hidden).unwrap());

    let all = super::TrackFilter::default();
    let performers = super::query_performers(&conn, &hidden, &all, super::Counted::Albums).unwrap();
    let listed: Vec<(PerformerKind, &str, &[String], u32)> = performers
        .iter()
        .map(|p| (p.kind, p.sort.as_str(), p.roles.as_slice(), p.count))
        .collect();
    let roles = |roles: &[&str]| roles.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        listed,
        [
            // Conducting on as many albums as playing solo
            (
                PerformerKind::Conductor,
                "Chandler, Adrian",
                roles(&["solo violin"]).as_slice(),
                2
            ),
            (PerformerKind::Conductor, "Herreweghe, Philippe", &[], 2),
            (
                PerformerKind::Ensemble,
                "Collegium Vocale Gent",
                roles(&["choir vocals"]).as_slice(),
                2
            ),
            // A singer, though the album artist named her on a track
            (
                PerformerKind::Soloist,
                "Eerens, Ilse",
                roles(&["soprano vocals"]).as_slice(),
                1
            ),
        ]
    );
    // The list's search looks into the names and the roles
    assert_eq!(
        performers[3].search,
        "ilse eerens eerens, ilse soprano vocals"
    );

    // A composer's performers are those of their tracks, with the
    // albums of those tracks, each as what they do the most anywhere
    let filter = super::TrackFilter {
        composer: Some("antonin dvorak".to_string()),
        ..Default::default()
    };
    let dvorak = super::query_performers(&conn, &hidden, &filter, super::Counted::Albums).unwrap();
    let listed: Vec<(PerformerKind, &str, u32)> = dvorak
        .iter()
        .map(|p| (p.kind, p.sort.as_str(), p.count))
        .collect();
    assert_eq!(
        listed,
        [
            (PerformerKind::Conductor, "Herreweghe, Philippe", 1),
            (PerformerKind::Ensemble, "Collegium Vocale Gent", 1),
            (PerformerKind::Soloist, "Eerens, Ilse", 1),
        ]
    );
    // and the roles they have on their other tracks too
    assert_eq!(dvorak[1].roles, performers[2].roles);

    // A performer's albums, whatever they do on them, with the tracks
    // that are theirs, and with a composer too
    let filter = super::TrackFilter {
        performer: Some("adrian chandler".to_string()),
        ..Default::default()
    };
    let mut albums: Vec<(String, (u32, u32))> = super::query_search_albums(&conn, &filter, &hidden)
        .unwrap()
        .into_iter()
        .collect();
    albums.sort();
    assert_eq!(
        albums,
        [("c".to_string(), (1, 1)), ("d".to_string(), (2, 2))]
    );
    let filter = super::TrackFilter {
        performer: Some("ilse eerens".to_string()),
        ..Default::default()
    };
    let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
    assert_eq!(albums.get("a"), Some(&(2, 2)));
    assert_eq!(albums.len(), 1);
    let filter = super::TrackFilter {
        composer: Some("antonin dvorak".to_string()),
        ..filter
    };
    let albums = super::query_search_albums(&conn, &filter, &hidden).unwrap();
    assert_eq!(albums.get("a"), Some(&(1, 2)));
}

#[test]
fn artists_are_listed_when_nothing_else_names_them() {
    use crate::audio::PerformerKind;

    let conn = test_db();
    let artist = |name: &str, sort: &str| format!("a\t{}\t{name}\t{sort}\t", name.to_lowercase());
    let kinks = artist("The Kinks", "Kinks, The");
    let ray = "s\tray davies\tRay Davies\tDavies, Ray\tlead vocals".to_string();
    let rostropovich_solo =
        "s\tmstislav rostropovich\tMstislav Rostropovich\tRostropovich, Mstislav\tcello"
            .to_string();
    let rostropovich = artist("Mstislav Rostropovich", "Mstislav Rostropovich");
    let zefiro_guessed = "e\tensemble zefiro\tEnsemble Zefiro\tEnsemble Zefiro\t".to_string();
    let zefiro = artist("Ensemble Zefiro", "Ensemble Zefiro");
    let luks = "c\tvaclav luks\tVáclav Luks\tLuks, Václav\t".to_string();
    let hendrix = artist("Jimi Hendrix Experience", "Jimi Hendrix Experience");
    let heartbreakers = artist(
        "Tom Petty and the Heartbreakers",
        "Tom Petty and the Heartbreakers",
    );
    let beatles = artist("The Beatles", "Beatles, The");
    let composer = |name: &str| Some(format!("\n{}\t{name}\t\n", name.to_lowercase()));
    let entries = |lines: &[&String]| {
        Some(format!(
            "\n{}\n",
            lines
                .iter()
                .map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        ))
    };
    let rows = [
        // A band, its singer credited on one album
        ("/m/K1/01.opus", "k1", entries(&[&kinks, &ray]), None, None),
        ("/m/K2/01.opus", "k2", entries(&[&kinks]), None, None),
        // A soloist, credited once, the album artist of two albums of
        // encores, stays a soloist
        (
            "/m/R1/01.opus",
            "r1",
            entries(&[&rostropovich_solo]),
            None,
            Some("w1"),
        ),
        ("/m/R2/01.opus", "r2", entries(&[&rostropovich]), None, None),
        ("/m/R3/01.opus", "r3", entries(&[&rostropovich]), None, None),
        // A group guessed from an album artist stays a group
        (
            "/m/Z1/01.opus",
            "z1",
            entries(&[&zefiro_guessed]),
            None,
            Some("w2"),
        ),
        ("/m/Z2/01.opus", "z2", entries(&[&zefiro]), None, None),
        // A composer as the album artist, whom others record more often
        (
            "/m/Zel/01.opus",
            "zel",
            entries(&[&artist("Jan Dismas Zelenka", "Jan Dismas Zelenka")]),
            None,
            None,
        ),
        (
            "/m/L1/01.opus",
            "l1",
            entries(&[&luks]),
            composer("Jan Dismas Zelenka"),
            Some("w3"),
        ),
        (
            "/m/L2/01.opus",
            "l2",
            entries(&[&luks]),
            composer("Jan Dismas Zelenka"),
            Some("w3"),
        ),
        // and one named with a group, recorded by others twice
        (
            "/m/BZ/01.opus",
            "bz",
            entries(&[&artist(
                "Ludwig van Beethoven, Ensemble Zefiro",
                "Ludwig van Beethoven, Ensemble Zefiro",
            )]),
            None,
            None,
        ),
        (
            "/m/L3/01.opus",
            "l3",
            entries(&[&luks]),
            composer("Ludwig van Beethoven"),
            Some("w4"),
        ),
        (
            "/m/L7/01.opus",
            "l7",
            entries(&[&luks]),
            composer("Ludwig van Beethoven"),
            Some("w4"),
        ),
        // A composer as the album artist of two albums, recorded once by
        // others, stays
        (
            "/m/Q1/01.opus",
            "q1",
            entries(&[&artist("Johann Joachim Quantz", "Johann Joachim Quantz")]),
            None,
            None,
        ),
        (
            "/m/Q2/01.opus",
            "q2",
            entries(&[&artist("Johann Joachim Quantz", "Johann Joachim Quantz")]),
            None,
            None,
        ),
        (
            "/m/L4/01.opus",
            "l4",
            entries(&[&luks]),
            composer("Johann Joachim Quantz"),
            Some("w5"),
        ),
        // A songwriter credited on their own albums, the album they made
        // with a band, and a cover tagged with only its artist, which
        // isn't evidence of others performing their songs
        (
            "/m/D1/01.opus",
            "d1",
            entries(&[&"s\tbob dylan\tBob Dylan\tDylan, Bob\tvocals".to_string()]),
            composer("Bob Dylan"),
            None,
        ),
        (
            "/m/D2/01.opus",
            "d2",
            entries(&[&"s\tbob dylan\tBob Dylan\tDylan, Bob\tvocals".to_string()]),
            composer("Bob Dylan"),
            None,
        ),
        (
            "/m/D3/01.opus",
            "d3",
            entries(&[&artist("Bob Dylan & The Band", "Bob Dylan & The Band")]),
            composer("Bob Dylan"),
            None,
        ),
        (
            "/m/H/01.opus",
            "h",
            entries(&[&hendrix]),
            composer("Bob Dylan"),
            None,
        ),
        // A composer as the album artist, whom others record under the
        // name MusicBrainz gives them, with their patronymic
        (
            "/m/AB/01.opus",
            "ab",
            entries(&[&artist("Alexander Borodin", "Alexander Borodin")]),
            None,
            None,
        ),
        (
            "/m/L5/01.opus",
            "l5",
            entries(&[&luks]),
            composer("Alexander Porfyrevich Borodin"),
            Some("w6"),
        ),
        (
            "/m/L6/01.opus",
            "l6",
            entries(&[&luks]),
            composer("Alexander Porfyrevich Borodin"),
            Some("w6"),
        ),
        // A singer who writes the songs of the band named after them
        (
            "/m/TP/01.opus",
            "tp",
            entries(&[&artist("Tom Petty", "Tom Petty")]),
            composer("Tom Petty"),
            None,
        ),
        (
            "/m/HB1/01.opus",
            "hb1",
            entries(&[&heartbreakers]),
            composer("Tom Petty"),
            None,
        ),
        (
            "/m/HB2/01.opus",
            "hb2",
            entries(&[&heartbreakers]),
            composer("Tom Petty"),
            None,
        ),
        // A band named after its singer, whose songs another singer
        // recorded once: he performs on the band's albums, so it stays
        (
            "/m/NC/01.opus",
            "nc",
            entries(&[&artist(
                "Nick Cave & the Bad Seeds",
                "Nick Cave & the Bad Seeds",
            )]),
            composer("Nick Cave"),
            None,
        ),
        (
            "/m/JC/01.opus",
            "jc",
            entries(&[&"s\tjohnny cash\tJohnny Cash\tCash, Johnny\tvocals".to_string()]),
            composer("Nick Cave"),
            None,
        ),
        // A songwriter whose band's albums, tagged with only their artist
        // and composers, outnumber their own, whether they're tagged as
        // iTunes tags them, or as Picard does, with a work for each song
        (
            "/m/PM/01.opus",
            "pm",
            entries(&[&artist("Paul McCartney", "McCartney, Paul")]),
            composer("Paul McCartney"),
            None,
        ),
        (
            "/m/B1/01.opus",
            "b1",
            entries(&[&beatles]),
            composer("Paul McCartney"),
            None,
        ),
        (
            "/m/B2/01.opus",
            "b2",
            entries(&[&beatles]),
            composer("Paul McCartney"),
            Some("w7"),
        ),
        (
            "/m/B3/01.opus",
            "b3",
            entries(&[&beatles]),
            composer("Paul McCartney"),
            Some("w8"),
        ),
        // A composer as the album artist, whose movements others record
        // on albums whose tags credit nobody, which are never given their
        // artist
        (
            "/m/AV/01.opus",
            "av",
            entries(&[&artist("Antonio Vivaldi", "Antonio Vivaldi")]),
            None,
            None,
        ),
        (
            "/m/M1/01.opus",
            "m1",
            None,
            composer("Antonio Vivaldi"),
            Some("w9"),
        ),
        (
            "/m/M2/01.opus",
            "m2",
            None,
            composer("Antonio Vivaldi"),
            Some("w9"),
        ),
    ];
    for (path, album, performers, composers, work_key) in rows {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, performers,
                                 composers, work_key, search)
             VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, ?5, '')",
            rusqlite::params![path.as_bytes(), album, performers, composers, work_key],
        )
        .unwrap();
    }

    let all = super::TrackFilter::default();
    let performers = super::query_performers(&conn, &[], &all, super::Counted::Albums).unwrap();
    let listed: Vec<(PerformerKind, &str, u32)> = performers
        .iter()
        .map(|p| (p.kind, p.sort.as_str(), p.count))
        .collect();
    assert_eq!(
        listed,
        [
            (PerformerKind::Conductor, "Luks, Václav", 7),
            (PerformerKind::Ensemble, "Ensemble Zefiro", 2),
            (PerformerKind::Soloist, "Cash, Johnny", 1),
            (PerformerKind::Soloist, "Davies, Ray", 1),
            (PerformerKind::Soloist, "Dylan, Bob", 2),
            (PerformerKind::Soloist, "Rostropovich, Mstislav", 3),
            (PerformerKind::Artist, "Beatles, The", 3),
            (PerformerKind::Artist, "Bob Dylan & The Band", 1),
            (PerformerKind::Artist, "Jimi Hendrix Experience", 1),
            (PerformerKind::Artist, "Johann Joachim Quantz", 2),
            (PerformerKind::Artist, "Kinks, The", 2),
            (PerformerKind::Artist, "McCartney, Paul", 1),
            (PerformerKind::Artist, "Nick Cave & the Bad Seeds", 1),
            (PerformerKind::Artist, "Tom Petty", 1),
            (PerformerKind::Artist, "Tom Petty and the Heartbreakers", 2),
        ]
    );

    // A band's albums
    let filter = super::TrackFilter {
        performer: Some("the kinks".to_string()),
        ..Default::default()
    };
    let mut albums: Vec<String> = super::query_search_albums(&conn, &filter, &[])
        .unwrap()
        .into_keys()
        .collect();
    albums.sort();
    assert_eq!(albums, ["k1", "k2"]);

    // On the Works page, only those of works: the artists of the songs
    // that Picard gives works, by the number of the works
    let works = super::query_performers(&conn, &[], &all, super::Counted::Works).unwrap();
    let artists: Vec<(&str, u32)> = works
        .iter()
        .filter(|p| p.kind == PerformerKind::Artist)
        .map(|p| (p.sort.as_str(), p.count))
        .collect();
    assert_eq!(artists, [("Beatles, The", 2)]);

    // A library with only artists has performers
    let conn = test_db();
    conn.execute(
        "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, performers, search)
         VALUES (?1, ?1, 0, 0, 0, 0, 'k1', ?2, '')",
        rusqlite::params![b"/m/K1/01.opus".as_slice(), entries(&[&kinks])],
    )
    .unwrap();
    assert!(super::query_tracks_have(&conn, "performers", &[]).unwrap());
}

#[test]
fn the_works_of_a_composer_are_in_the_order_of_their_catalogue() {
    use super::TrackFilter;

    let conn = test_db();
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    let beethoven = "\nludwig van beethoven\tLudwig van Beethoven\tBeethoven, Ludwig van\n";
    let dvorak = "\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n";
    for (i, (name, composers)) in [
        ("Suite no. 1 in G major, BWV 1007", bach),
        ("A chorale without a number", bach),
        ("Partita no. 2 in D minor, BWV 1004", bach),
        ("Für Elise, WoO 59", beethoven),
        ("Symphony no. 3, op. 55 “Eroica”", beethoven),
        ("Piano Sonata no. 5, op. 10 no. 1", beethoven),
        ("Piano Sonata no. 1, op. 2 no. 1", beethoven),
        // Opus numbers are the most common, so the polka, with only a
        // B. number, comes after them
        ("Polka, B. 1", dvorak),
        ("Humoresques, op. 101", dvorak),
        ("Symphony no. 9, op. 95", dvorak),
        ("Slavonic Dances, op. 46, B. 83", dvorak),
    ]
    .into_iter()
    .enumerate()
    {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, work_key,
                                     work, work_composers)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?3, ?4)",
            rusqlite::params![
                format!("/m/{i}/01.flac").as_bytes(),
                format!("album {i}"),
                name,
                composers
            ],
        )
        .unwrap();
    }

    let works = super::query_works(&conn, &TrackFilter::default(), &[]).unwrap();
    let names: Vec<_> = works.iter().map(|work| work.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Partita no. 2 in D minor, BWV 1004",
            "Suite no. 1 in G major, BWV 1007",
            "A chorale without a number",
            "Piano Sonata no. 1, op. 2 no. 1",
            "Piano Sonata no. 5, op. 10 no. 1",
            "Symphony no. 3, op. 55 “Eroica”",
            "Für Elise, WoO 59",
            "Slavonic Dances, op. 46, B. 83",
            "Symphony no. 9, op. 95",
            "Humoresques, op. 101",
            "Polka, B. 1",
        ]
    );
}

#[test]
fn arrangements_are_listed_with_what_they_are() {
    use super::TrackFilter;

    let conn = test_db();
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    let partita = "Violin Partita no. 2 in D minor, BWV 1004";
    for (path, album, work, arrangement_of, arrangement) in [
        ("/m/Violin/05.flac", "violin", "partita", None, None),
        // The catch-all for its arrangements, and one of its own
        (
            "/m/Lute/20.flac",
            "lute",
            "catch-all",
            Some(partita),
            Some(""),
        ),
        (
            "/m/Piano/01.flac",
            "piano",
            "busoni",
            Some(partita),
            Some("for piano, Busoni"),
        ),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, work_key,
                                     work, work_composers, movement_order,
                                     work_arrangement_of, work_arrangement)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4, ?5, '5', ?6, ?7)",
            rusqlite::params![
                path.as_bytes(),
                album,
                work,
                partita,
                bach,
                arrangement_of,
                arrangement
            ],
        )
        .unwrap();
    }

    let works = super::query_works(&conn, &TrackFilter::default(), &[]).unwrap();
    let labels: Vec<_> = works
        .iter()
        .map(|work| {
            (
                work.key.as_str(),
                work.arrangement.as_ref().map(super::Arrangement::shown),
            )
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("busoni", Some("for piano, Busoni".to_string())),
            ("catch-all", Some("arrangement".to_string())),
            ("partita", None),
        ]
    );
}

#[test]
fn arrangements_are_listed_under_the_works_they_arrange() {
    use super::TrackFilter;

    let conn = test_db();
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    let schubert = "\nfranz schubert\tFranz Schubert\tSchubert, Franz\n";
    let liszt = "\nfranz liszt\tFranz Liszt\tLiszt, Franz\n";
    let barber = "\nsamuel barber\tSamuel Barber\tBarber, Samuel\n";
    // (path, track, key, composers, order, the id of the work it
    // arranges, what it is, the ids of its parts, search)
    type Row<'a> = (
        &'a str,
        u32,
        &'a str,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        &'a str,
        Option<&'a str>,
        &'a str,
    );
    let rows: &[Row] = &[
        // A partita, and the catch-all for its arrangements, whole in a
        // track after it on the same album and on an album of its own
        (
            "/m/violin/01",
            1,
            "partita",
            bach,
            Some("1"),
            None,
            "",
            Some("allemande"),
            "",
        ),
        (
            "/m/violin/02",
            2,
            "partita",
            bach,
            Some("5"),
            None,
            "",
            Some("chaconne"),
            "",
        ),
        (
            "/m/violin/03",
            3,
            "lute",
            bach,
            None,
            Some("partita"),
            "",
            None,
            "",
        ),
        (
            "/m/lute/01",
            1,
            "lute",
            bach,
            None,
            Some("partita"),
            "",
            None,
            "",
        ),
        // An arrangement of its own, and an arrangement of that one
        (
            "/m/piano/01",
            1,
            "busoni",
            bach,
            None,
            Some("partita"),
            "for piano, Busoni",
            None,
            "",
        ),
        (
            "/m/pianos/01",
            1,
            "pianos",
            bach,
            None,
            Some("busoni"),
            "for two pianos",
            None,
            "",
        ),
        // A song of a cycle arranged for a piano trio, which the tags
        // make a movement of the cycle, and a paraphrase of it credited
        // to the arranger, which stays on its own
        (
            "/m/lieder/04",
            4,
            "d957",
            schubert,
            Some("4"),
            None,
            "",
            Some("standchen"),
            "",
        ),
        (
            "/m/trio/13",
            13,
            "standchen",
            schubert,
            Some("4"),
            Some("standchen"),
            "",
            None,
            "trio",
        ),
        (
            "/m/liszt/07",
            7,
            "paraphrase",
            liszt,
            Some("4"),
            Some("standchen"),
            "",
            None,
            "",
        ),
        // A whole work that arranges a movement stays on its own
        (
            "/m/quartet/02",
            2,
            "op11",
            barber,
            Some("2"),
            None,
            "",
            Some("molto"),
            "",
        ),
        (
            "/m/strings/01",
            1,
            "adagio",
            barber,
            None,
            Some("molto"),
            "",
            None,
            "",
        ),
        // An arrangement whose original isn't in the library
        (
            "/m/other/01",
            1,
            "orphan",
            bach,
            None,
            Some("missing"),
            "",
            None,
            "",
        ),
        // A recording whose last movement MusicBrainz links to an
        // arrangement of the first
        (
            "/m/sonn/01",
            1,
            "bwv80",
            bach,
            Some("1"),
            None,
            "",
            Some("bwv80-1"),
            "",
        ),
        (
            "/m/sonn/02",
            2,
            "bwv80",
            bach,
            Some("2"),
            None,
            "",
            Some("bwv80-2"),
            "",
        ),
        (
            "/m/sonn/08",
            8,
            "chorale",
            bach,
            Some("8"),
            Some("bwv80-1"),
            "",
            None,
            "",
        ),
        // A song in two works, which an arrangement of it goes under
        // the first of by key, every time
        (
            "/m/cycle/01",
            1,
            "cycle",
            schubert,
            Some("1"),
            None,
            "",
            Some("song"),
            "",
        ),
        (
            "/m/set/01",
            1,
            "collection",
            schubert,
            Some("1"),
            None,
            "",
            Some("song"),
            "",
        ),
        (
            "/m/song/01",
            1,
            "song-arr",
            schubert,
            Some("1"),
            Some("song"),
            "",
            None,
            "",
        ),
        // Two arrangements of each other
        (
            "/m/loop/01",
            1,
            "loop-a",
            bach,
            None,
            Some("loop-b"),
            "",
            None,
            "",
        ),
        (
            "/m/loop/02",
            2,
            "loop-b",
            bach,
            None,
            Some("loop-a"),
            "",
            None,
            "",
        ),
    ];
    for (path, track, key, composers, order, of_id, label, parts, search) in rows {
        let album = path.split('/').nth(2).unwrap();
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album, track,
                                     search, work_key, work, work_composers, movement_order,
                                     work_arrangement_of, work_arrangement,
                                     work_arrangement_of_id, work_part_ids)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                path.as_bytes(),
                album,
                track,
                search,
                key,
                composers,
                order,
                of_id.map(|_| "the work it arranges"),
                of_id.map(|_| *label),
                of_id,
                parts
            ],
        )
        .unwrap();
    }
    let works = |filter: &TrackFilter| {
        let mut works = super::query_works(&conn, filter, &[])
            .unwrap()
            .into_iter()
            .map(|work| (work.key, work.recordings, work.arrangements))
            .collect::<Vec<_>>();
        works.sort();
        works
    };
    let strings = |keys: &[&str]| keys.iter().map(ToString::to_string).collect::<Vec<_>>();
    let work = |key: &str, recordings, arrangements: &[&str]| {
        (key.to_string(), recordings, strings(arrangements))
    };
    assert_eq!(
        works(&TrackFilter::default()),
        [
            work("adagio", 1, &[]),
            work("bwv80", 1, &["chorale"]),
            work("collection", 2, &["song-arr"]),
            work("cycle", 1, &[]),
            work("d957", 2, &["standchen"]),
            work("loop-a", 1, &[]),
            work("loop-b", 1, &[]),
            work("op11", 1, &[]),
            work("orphan", 1, &[]),
            work("paraphrase", 1, &[]),
            work("partita", 5, &["busoni", "lute", "pianos"]),
        ]
    );
    // A search that only finds the arrangement finds the work it's
    // listed under
    let trio = TrackFilter {
        query: super::TrackFilter::query("trio"),
        ..Default::default()
    };
    assert_eq!(works(&trio), [work("d957", 1, &["standchen"])]);

    // The recordings of a work and of its arrangements, each found by the
    // work of its own tracks, and saying what it is if it's all of an
    // arrangement; the movements of the arrangement are one recording
    // with those of the work, but the whole works are recordings of
    // their own
    let recordings = |keys: &[&str]| {
        super::query_work_recordings(&conn, &strings(keys), &TrackFilter::default(), &[])
            .unwrap()
            .into_iter()
            .map(|recording| {
                let paths = recording
                    .paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                let arrangement = recording.arrangement.map(|a| a.shown());
                (paths, recording.work_key, arrangement)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        recordings(&["bwv80", "chorale"]),
        [(
            strings(&["/m/sonn/01", "/m/sonn/02", "/m/sonn/08"]),
            "bwv80".to_string(),
            None
        )]
    );
    let mut partita = recordings(&["partita", "busoni", "lute", "pianos"]);
    partita.sort();
    assert_eq!(
        partita,
        [
            (
                strings(&["/m/lute/01"]),
                "lute".to_string(),
                Some("arrangement".to_string())
            ),
            (
                strings(&["/m/piano/01"]),
                "busoni".to_string(),
                Some("for piano, Busoni".to_string())
            ),
            (
                strings(&["/m/pianos/01"]),
                "pianos".to_string(),
                Some("for two pianos".to_string())
            ),
            (
                strings(&["/m/violin/01", "/m/violin/02"]),
                "partita".to_string(),
                None
            ),
            (
                strings(&["/m/violin/03"]),
                "lute".to_string(),
                Some("arrangement".to_string())
            ),
        ]
    );
    // The covers of the work take the tracks of its arrangements
    let partita = TrackFilter {
        works: strings(&["partita", "busoni", "lute", "pianos"]),
        ..Default::default()
    };
    let mut albums = super::query_search_albums(&conn, &partita, &[])
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    albums.sort();
    assert_eq!(
        albums,
        [
            ("lute".to_string(), (1, 1)),
            ("piano".to_string(), (1, 1)),
            ("pianos".to_string(), (1, 1)),
            ("violin".to_string(), (3, 3)),
        ]
    );

    // Play a Random Work picks from the works as they're listed, each
    // with the recordings that `work_recordings()` gives for its keys
    let sorted_paths = |recordings: Vec<(Vec<String>, String, Option<String>)>| {
        let mut recordings = recordings;
        recordings.sort();
        recordings
    };
    let strings_of = |paths: &[std::path::PathBuf]| {
        paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    for filter in [TrackFilter::default(), trio.clone()] {
        let candidates = super::query_work_recording_candidates(&conn, &filter, &[]).unwrap();
        let listed = super::query_works(&conn, &filter, &[]).unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| &candidate.info)
                .collect::<Vec<_>>(),
            listed.iter().collect::<Vec<_>>()
        );
        for candidate in &candidates {
            assert_eq!(
                candidate.recordings.len(),
                candidate.info.recordings as usize
            );
            // The same tracks, of the same works, which are as much
            // arrangements
            let expected =
                super::query_work_recordings(&conn, &candidate.info.keys(), &filter, &[])
                    .unwrap()
                    .into_iter()
                    .map(|recording| {
                        let arrangement = recording.arrangement.map(|a| a.shown());
                        (
                            strings_of(&recording.paths),
                            recording.work_key,
                            arrangement,
                        )
                    })
                    .collect();
            let found = candidate
                .recordings
                .iter()
                .map(|recording| {
                    let arrangement = recording
                        .arrangement
                        .as_ref()
                        .map(super::Arrangement::shown);
                    let work_key = recording.work_key.clone();
                    (strings_of(&recording.paths), work_key, arrangement)
                })
                .collect();
            assert_eq!(
                sorted_paths(found),
                sorted_paths(expected),
                "{}",
                candidate.info.key
            );
        }
    }
    let candidates =
        super::query_work_recording_candidates(&conn, &TrackFilter::default(), &[]).unwrap();
    let partita = candidates
        .iter()
        .find(|candidate| candidate.info.key == "partita")
        .unwrap();
    assert_eq!(partita.recordings.len(), 5);
    // The arrangement on the lute goes with the work, as it's listed
    // under it
    assert!(partita.recordings.iter().any(|recording| {
        recording.work_key == "lute" && strings_of(&recording.paths) == ["/m/violin/03"]
    }));
    // The work of a search that only finds an arrangement plays the
    // arrangement, and the tracks of all its recordings tell when it was
    // heard
    let candidates = super::query_work_recording_candidates(&conn, &trio, &[]).unwrap();
    let [d957] = candidates.as_slice() else {
        panic!("{candidates:?}");
    };
    assert_eq!(d957.info.key, "d957");
    let [recording] = d957.recordings.as_slice() else {
        panic!("{d957:?}");
    };
    assert_eq!(recording.work_key, "standchen");
    assert!(d957.paths.len() > recording.paths.len());
}

#[test]
fn works_are_listed_with_their_recordings() {
    let conn = test_db();
    let dvorak = "\nantonin dvorak\tAntonín Dvořák\tDvořák, Antonín\n";
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    let herreweghe = "\nc\tphilippe herreweghe\tPhilippe Herreweghe\tHerreweghe, Philippe\t\n";
    let eerens = "\ns\tilse eerens\tIlse Eerens\tEerens, Ilse\tsoprano vocals\n";
    // (path, album, year, disc, track, work, name, composers, order, performers, search)
    type Row<'a> = (
        &'a str,
        &'a str,
        Option<&'a str>,
        u32,
        u32,
        Option<&'a str>,
        &'a str,
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        &'a str,
    );
    let rows: &[Row] = &[
        // A Requiem, a track of something else, and a whole work
        (
            "/m/A/01",
            "a",
            Some("2015"),
            1,
            1,
            Some("req"),
            "Requiem, op. 89",
            dvorak,
            Some("1.1"),
            Some(herreweghe),
            "introitus",
        ),
        (
            "/m/A/02",
            "a",
            Some("2015"),
            1,
            2,
            Some("req"),
            "Requiem, op. 89",
            dvorak,
            Some("1.2"),
            Some(eerens),
            "graduale",
        ),
        (
            "/m/A/03",
            "a",
            Some("2015"),
            1,
            3,
            None,
            "",
            "",
            None,
            None,
            "interlude",
        ),
        (
            "/m/A/04",
            "a",
            Some("2015"),
            1,
            4,
            Some("sm"),
            "Stabat Mater",
            dvorak,
            None,
            None,
            "stabat",
        ),
        // A movement in two tracks, and a movement that comes again,
        // without dates or performers that show another performance,
        // which is still the same recording
        (
            "/m/B/01",
            "b",
            None,
            1,
            1,
            Some("gold"),
            "Goldberg Variations",
            bach,
            Some("1"),
            None,
            "aria",
        ),
        (
            "/m/B/02",
            "b",
            None,
            1,
            2,
            Some("gold"),
            "Goldberg Variations",
            bach,
            Some("2"),
            None,
            "variatio 1",
        ),
        (
            "/m/B/03",
            "b",
            None,
            1,
            3,
            Some("gold"),
            "Goldberg Variations",
            bach,
            Some("2"),
            None,
            "variatio 1 part 2",
        ),
        (
            "/m/B/04",
            "b",
            None,
            2,
            1,
            Some("gold"),
            "Goldberg Variations",
            bach,
            Some("1"),
            None,
            "aria",
        ),
        // Movements without their places
        (
            "/m/C/01",
            "c",
            Some("1995"),
            1,
            1,
            Some("req"),
            "Requiem v B-moll",
            dvorak,
            Some(""),
            Some(herreweghe),
            "kyrie",
        ),
        (
            "/m/C/02",
            "c",
            Some("1995"),
            1,
            2,
            Some("req"),
            "Requiem v B-moll",
            dvorak,
            Some(""),
            None,
            "agnus",
        ),
        // Out of the order of the work, and mixed with another
        (
            "/m/E/01",
            "e",
            None,
            1,
            1,
            Some("kdf"),
            "Die Kunst der Fuge",
            bach,
            Some("1"),
            None,
            "",
        ),
        (
            "/m/E/02",
            "e",
            None,
            1,
            2,
            Some("kdf"),
            "Die Kunst der Fuge",
            bach,
            Some("3"),
            None,
            "",
        ),
        (
            "/m/E/03",
            "e",
            None,
            1,
            3,
            Some("gold"),
            "Goldberg Variations",
            bach,
            Some("1"),
            None,
            "",
        ),
        (
            "/m/E/04",
            "e",
            None,
            1,
            4,
            Some("kdf"),
            "Die Kunst der Fuge",
            bach,
            Some("2"),
            None,
            "",
        ),
        // Linked to the whole work rather than to a movement of it
        (
            "/m/E/05",
            "e",
            None,
            1,
            5,
            Some("kdf"),
            "Die Kunst der Fuge",
            bach,
            None,
            None,
            "",
        ),
        // Numbers in order
        (
            "/m/D/01",
            "d",
            None,
            1,
            1,
            Some("s10"),
            "Symphony no. 10",
            dvorak,
            None,
            None,
            "",
        ),
        (
            "/m/D/02",
            "d",
            None,
            1,
            2,
            Some("s2"),
            "Symphony no. 2",
            dvorak,
            None,
            None,
            "",
        ),
        (
            "/m/Gone/01",
            "g",
            None,
            1,
            1,
            Some("req"),
            "Requiem, op. 89",
            dvorak,
            Some("1.1"),
            None,
            "gone",
        ),
    ];
    for (path, album, year, disc, track, key, name, composers, order, performers, search) in rows {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                     recorded_sort_date, disc, track, search, work_key, work,
                                     work_composers, movement_order, performers)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                path.as_bytes(),
                album,
                year.and_then(crate::tag_date::TagDate::parse)
                    .map(crate::tag_date::TagDate::sort_text),
                disc,
                track,
                search,
                key,
                key.map(|_| *name),
                key.map(|_| *composers),
                order,
                performers
            ],
        )
        .unwrap();
    }
    let hidden = [PathBuf::from("/m/Gone")];
    let works = |filter: &super::TrackFilter| {
        super::query_works(&conn, filter, &hidden)
            .unwrap()
            .into_iter()
            .map(|work| (work.name, work.recordings))
            .collect::<Vec<_>>()
    };
    let all = super::TrackFilter::default();
    // By the composers' sort names, then by name, and the name most of
    // the tracks give
    assert_eq!(
        works(&all),
        [
            ("Die Kunst der Fuge".to_string(), 1),
            ("Goldberg Variations".to_string(), 2),
            ("Requiem v B-moll".to_string(), 2),
            ("Stabat Mater".to_string(), 1),
            ("Symphony no. 2".to_string(), 1),
            ("Symphony no. 10".to_string(), 1),
        ]
    );
    // The recordings with a track the search matches
    let graduale = super::TrackFilter {
        query: super::TrackFilter::query("graduale"),
        ..Default::default()
    };
    assert_eq!(works(&graduale), [("Requiem v B-moll".to_string(), 1)]);
    let herreweghe_filter = super::TrackFilter {
        performer: Some("philippe herreweghe".to_string()),
        ..Default::default()
    };
    assert_eq!(
        works(&herreweghe_filter),
        [("Requiem v B-moll".to_string(), 2)]
    );

    // The recordings, each with all its tracks and the performers of
    // all of them, by year, those without one last
    let recordings = |key: &str, filter: &super::TrackFilter| {
        super::query_work_recordings(&conn, &[key.to_string()], filter, &hidden)
            .unwrap()
            .into_iter()
            .map(|recording| {
                let paths = recording
                    .paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                (recording.year, paths, recording.performers)
            })
            .collect::<Vec<_>>()
    };
    let strings = |paths: &[&str]| paths.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        recordings("req", &all),
        [
            (
                Some("1995".to_string()),
                strings(&["/m/C/01", "/m/C/02"]),
                "Philippe Herreweghe".to_string()
            ),
            (
                Some("2015".to_string()),
                strings(&["/m/A/01", "/m/A/02"]),
                "Philippe Herreweghe · Ilse Eerens".to_string()
            ),
        ]
    );
    assert_eq!(
        recordings("req", &graduale),
        [(
            Some("2015".to_string()),
            strings(&["/m/A/01", "/m/A/02"]),
            "Philippe Herreweghe · Ilse Eerens".to_string()
        )]
    );
    assert_eq!(
        recordings("gold", &all),
        [
            (
                None,
                strings(&["/m/B/01", "/m/B/02", "/m/B/03", "/m/B/04"]),
                String::new()
            ),
            (None, strings(&["/m/E/03"]), String::new()),
        ]
    );
    // The tracks of a work on each album, as the covers of the albums of
    // its recordings take them, alone and with the words of a search
    let gold = super::TrackFilter {
        works: vec!["gold".to_string()],
        ..Default::default()
    };
    assert!(!gold.is_empty());
    let albums = |filter: &super::TrackFilter| {
        let mut albums = super::query_search_albums(&conn, filter, &hidden)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        albums.sort();
        albums
    };
    assert_eq!(
        albums(&gold),
        [("b".to_string(), (4, 4)), ("e".to_string(), (1, 5))]
    );
    let variations = super::TrackFilter {
        query: super::TrackFilter::query("variatio"),
        ..gold.clone()
    };
    assert_eq!(albums(&variations), [("b".to_string(), (2, 4))]);
    let tracks = super::query_album_tracks(&conn, "e", &gold, &hidden)
        .unwrap()
        .into_iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(tracks, strings(&["/m/E/03"]));
    // The tracks of a recording, as its cover takes them, and none of
    // an empty one
    let recording = super::TrackFilter {
        tracks: Some(std::sync::Arc::from([
            PathBuf::from("/m/B/01"),
            PathBuf::from("/m/B/02"),
        ])),
        ..Default::default()
    };
    assert!(!recording.is_empty());
    assert_eq!(albums(&recording), [("b".to_string(), (2, 4))]);
    let tracks = super::query_album_tracks(&conn, "b", &recording, &hidden)
        .unwrap()
        .into_iter()
        .map(|(_, path)| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(tracks, strings(&["/m/B/01", "/m/B/02"]));
    let nothing = super::TrackFilter {
        tracks: Some(std::sync::Arc::from([])),
        ..Default::default()
    };
    assert_eq!(albums(&nothing), []);

    // The work of a track
    let work = |path: &str| super::query_track_work(&conn, Path::new(path), &hidden).unwrap();
    assert_eq!(work("/m/B/04").as_deref(), Some("gold"));
    assert_eq!(work("/m/A/01").as_deref(), Some("req"));
    assert_eq!(work("/m/A/03"), None);
    assert_eq!(work("/m/Gone/01").as_deref(), Some("req"));
    assert_eq!(
        recordings("kdf", &all),
        [(
            None,
            strings(&["/m/E/01", "/m/E/02", "/m/E/04", "/m/E/05"]),
            String::new()
        )]
    );

    // The works as Play a Random Work picks from them, each with the
    // recordings of it that the filter matches
    let candidates = |filter: &super::TrackFilter| {
        super::query_work_recording_candidates(&conn, filter, &hidden)
            .unwrap()
            .into_iter()
            .map(|work| {
                let paths = |paths: &[PathBuf]| {
                    paths
                        .iter()
                        .map(|path| path.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                };
                let recordings = work
                    .recordings
                    .iter()
                    .map(|recording| paths(&recording.paths))
                    .collect::<Vec<_>>();
                (work.info.name, recordings, paths(&work.paths))
            })
            .collect::<Vec<_>>()
    };
    let kdf = strings(&["/m/E/01", "/m/E/02", "/m/E/04", "/m/E/05"]);
    let goldberg = strings(&["/m/B/01", "/m/B/02", "/m/B/03", "/m/B/04", "/m/E/03"]);
    let requiem = strings(&["/m/A/01", "/m/A/02", "/m/C/01", "/m/C/02"]);
    assert_eq!(
        candidates(&all),
        [
            ("Die Kunst der Fuge".to_string(), vec![kdf.clone()], kdf),
            (
                "Goldberg Variations".to_string(),
                vec![
                    strings(&["/m/B/01", "/m/B/02", "/m/B/03", "/m/B/04"]),
                    strings(&["/m/E/03"]),
                ],
                goldberg
            ),
            (
                "Requiem v B-moll".to_string(),
                vec![
                    strings(&["/m/A/01", "/m/A/02"]),
                    strings(&["/m/C/01", "/m/C/02"])
                ],
                requiem.clone()
            ),
            (
                "Stabat Mater".to_string(),
                vec![strings(&["/m/A/04"])],
                strings(&["/m/A/04"])
            ),
            (
                "Symphony no. 2".to_string(),
                vec![strings(&["/m/D/02"])],
                strings(&["/m/D/02"])
            ),
            (
                "Symphony no. 10".to_string(),
                vec![strings(&["/m/D/01"])],
                strings(&["/m/D/01"])
            ),
        ]
    );
    // Only the recordings the search matches, but the tracks of all of
    // them, which tell when the work was heard last
    assert_eq!(
        candidates(&graduale),
        [(
            "Requiem v B-moll".to_string(),
            vec![strings(&["/m/A/01", "/m/A/02"])],
            requiem
        )]
    );
    // The recordings that are a single movement of their work: not two
    // movements, nor the parts of one with another, nor movements whose
    // places aren't known, nor a whole work
    let single_movements = super::query_work_recording_candidates(&conn, &all, &hidden)
        .unwrap()
        .into_iter()
        .flat_map(|work| work.recordings)
        .filter(|recording| recording.single_movement)
        .map(|recording| {
            recording
                .paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(single_movements, [strings(&["/m/E/03"])]);
}

#[test]
fn export_infos_read_title_artist_and_length() {
    let conn = test_db();
    for (path, title, artist, fields) in [
        (
            "/m/A/01.flac",
            Some("Intro"),
            Some("Band"),
            Some("ln\t300\n"),
        ),
        ("/m/A/Album.cue/track02", Some("Two"), None, Some("ln\t0\n")),
        ("/m/A/03.flac", None, None, None),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, title, artist, fields)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?3, ?4)",
            rusqlite::params![path.as_bytes(), title, artist, fields],
        )
        .unwrap();
    }
    let paths = [
        "/m/A/01.flac",
        "/m/A/Album.cue/track02",
        "/m/A/03.flac",
        "/m/Gone.flac",
    ]
    .map(PathBuf::from);
    let infos = super::query_export_infos(&conn, &paths).unwrap();
    assert_eq!(
        infos[&paths[0]],
        super::ExportInfo {
            title: Some("Intro".to_string()),
            artist: Some("Band".to_string()),
            length: Some(300),
        }
    );
    // A length of 0 isn't known
    assert_eq!(infos[&paths[1]].length, None);
    assert_eq!(infos[&paths[2]], super::ExportInfo::default());
    assert!(!infos.contains_key(&paths[3]));
}

#[test]
fn a_recording_is_as_long_as_its_tracks() {
    let conn = test_db();
    let length = |fields: Option<&str>| -> Option<i64> {
        conn.query_row("SELECT dismas_length(?1)", [fields], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(length(Some("ln\t300\nsr\t44100\n")), Some(300));
    assert_eq!(length(Some("ti\tadagio\nln\t42\nsr\t44100\n")), Some(42));
    assert_eq!(length(Some("ti\tadagio\nln\t7")), Some(7));
    assert_eq!(length(Some("ti\tadagio\n")), None);
    assert_eq!(length(None), None);

    // A movement whose length isn't known counts as long as the others
    // of its recording; a recording with no length known has none
    for (path, album, order, fields) in [
        ("/m/A/01", "a", "1", Some("ti\tlargo\nln\t100\n")),
        ("/m/A/02", "a", "2", Some("ln\t200\n")),
        ("/m/A/03", "a", "3", None),
        ("/m/B/01", "b", "1", None),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                     work_key, work, movement_order, fields)
                 VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, 'w', 'Work', ?3, ?4)",
            rusqlite::params![path.as_bytes(), album, order, fields],
        )
        .unwrap();
    }
    let candidates =
        super::query_work_recording_candidates(&conn, &super::TrackFilter::default(), &[]).unwrap();
    let [work] = candidates.as_slice() else {
        panic!("{candidates:?}");
    };
    let lengths = work
        .recordings
        .iter()
        .map(|recording| recording.length)
        .collect::<Vec<_>>();
    assert_eq!(lengths, [Some(450), None]);
}

#[test]
fn a_work_played_again_on_an_album_is_split_only_by_another_date_or_other_performers() {
    let conn = test_db();
    let bach = "\njohann sebastian bach\tJohann Sebastian Bach\tBach, Johann Sebastian\n";
    // A track: its work, the place of its movement, the keys of its
    // performers, and when it was recorded. "arr" arranges "orig"
    type Track<'a> = (&'a str, &'a str, &'a [&'a str], Option<&'a str>);
    let albums: &[(&str, &[Track])] = &[
        // Played again eleven years later, by the same pianist
        (
            "remake",
            &[
                ("remake", "1", &["p"], Some("1990")),
                ("remake", "2", &["p"], Some("1990")),
                ("remake", "3", &["p"], Some("1990")),
                ("remake", "1", &["p"], Some("2001")),
                ("remake", "2", &["p"], Some("2001")),
                ("remake", "3", &["p"], Some("2001")),
            ],
        ),
        // Two pianists, without dates
        (
            "pianists",
            &[
                ("pianists", "1", &["a"], None),
                ("pianists", "2", &["a"], None),
                ("pianists", "1", &["b"], None),
                ("pianists", "2", &["b"], None),
            ],
        ),
        // A da capo, by the same players at the same time
        (
            "da-capo",
            &[
                ("da-capo", "1", &["a"], Some("2024-02")),
                ("da-capo", "2", &["a"], Some("2024-02")),
                ("da-capo", "1", &["a"], Some("2024-02")),
            ],
        ),
        // Sessions less than a year apart
        (
            "sessions",
            &[
                ("sessions", "1", &[], Some("1997-04/1997-09")),
                ("sessions", "2", &[], Some("1997-04/1997-09")),
                ("sessions", "1", &[], Some("1998-03")),
            ],
        ),
        // A date on one side only
        (
            "one-date",
            &[
                ("one-date", "1", &[], None),
                ("one-date", "2", &[], None),
                ("one-date", "1", &[], Some("2001")),
            ],
        ),
        // A performer in common, without dates
        (
            "in-common",
            &[
                ("in-common", "1", &["a", "b"], None),
                ("in-common", "2", &["a", "b"], None),
                ("in-common", "1", &["b", "c"], None),
            ],
        ),
        // The place of the track before, as the parts of a movement in
        // two tracks have, whatever their tags say
        (
            "parts",
            &[
                ("parts", "1", &["a"], Some("1990")),
                ("parts", "2", &["a"], Some("1990")),
                ("parts", "2", &["b"], Some("2001")),
            ],
        ),
        // The years next to each other aren't a year apart, and those
        // two years apart are
        (
            "next-year",
            &[
                ("next-year", "1", &[], Some("1990")),
                ("next-year", "2", &[], Some("1990")),
                ("next-year", "1", &[], Some("1991")),
            ],
        ),
        (
            "two-years",
            &[
                ("two-years", "1", &[], Some("1990")),
                ("two-years", "2", &[], Some("1990")),
                ("two-years", "1", &[], Some("1992")),
            ],
        ),
        // The movements of an arrangement go with those of the work it's
        // listed under, so they come again there: by the same players,
        // it's the same recording, and by others, another
        (
            "arranged",
            &[
                ("orig", "1", &["a"], None),
                ("orig", "2", &["a"], None),
                ("arr", "1", &["a"], None),
                ("arr", "2", &["a"], None),
            ],
        ),
        (
            "arranged-apart",
            &[
                ("orig", "1", &["a"], None),
                ("orig", "2", &["a"], None),
                ("arr", "1", &["b"], None),
                ("arr", "2", &["b"], None),
            ],
        ),
    ];
    for (album, tracks) in albums {
        for (n, (work, place, performers, recorded)) in (1u32..).zip(tracks.iter()) {
            let path = format!("/m/{album}/{n:02}");
            // The lines of `performer_entries()`, as soloists
            let performers = (!performers.is_empty()).then(|| {
                performers
                    .iter()
                    .map(|key| format!("\ns\t{key}\t{key}\t{key}\t"))
                    .collect::<String>()
                    + "\n"
            });
            let arranges = (*work == "arr").then_some("orig");
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                         disc, track, work_key, work, work_composers,
                                         movement_order, performers, fields,
                                         work_arrangement_of, work_arrangement_of_id)
                     VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, 1, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
                rusqlite::params![
                    path.as_bytes(),
                    album,
                    n,
                    work,
                    bach,
                    place,
                    performers,
                    recorded.map(|date| format!("rd\t{date}\n")),
                    arranges,
                ],
            )
            .unwrap();
        }
    }

    // The recordings of each work, by the numbers of their tracks on
    // their albums
    type Recordings<'a> = &'a [(&'a str, &'a [u32])];
    let expected: &[(&[&str], Recordings)] = &[
        (
            &["remake"],
            &[("remake", &[1, 2, 3]), ("remake", &[4, 5, 6])],
        ),
        (
            &["pianists"],
            &[("pianists", &[1, 2]), ("pianists", &[3, 4])],
        ),
        (&["da-capo"], &[("da-capo", &[1, 2, 3])]),
        (&["sessions"], &[("sessions", &[1, 2, 3])]),
        (&["one-date"], &[("one-date", &[1, 2, 3])]),
        (&["in-common"], &[("in-common", &[1, 2, 3])]),
        (&["parts"], &[("parts", &[1, 2, 3])]),
        (&["next-year"], &[("next-year", &[1, 2, 3])]),
        (
            &["two-years"],
            &[("two-years", &[1, 2]), ("two-years", &[3])],
        ),
        (
            &["orig", "arr"],
            &[
                ("arranged", &[1, 2, 3, 4]),
                ("arranged-apart", &[1, 2]),
                ("arranged-apart", &[3, 4]),
            ],
        ),
    ];
    let all = super::TrackFilter::default();
    let strings = |paths: &[PathBuf]| {
        paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    for (keys, recordings) in expected {
        let keys: Vec<String> = keys.iter().map(ToString::to_string).collect();
        let found = super::query_work_recordings(&conn, &keys, &all, &[])
            .unwrap()
            .into_iter()
            .map(|recording| strings(&recording.paths))
            .collect::<Vec<_>>();
        let wanted = recordings
            .iter()
            .map(|(album, tracks)| {
                tracks
                    .iter()
                    .map(|n| format!("/m/{album}/{n:02}"))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(found, wanted, "{keys:?}");
    }

    // The rows count them, as Play a Random Work picks from them; the
    // arrangement is listed under the work
    let mut works = super::query_works(&conn, &all, &[])
        .unwrap()
        .into_iter()
        .map(|work| (work.key, work.recordings))
        .collect::<Vec<_>>();
    works.sort();
    let mut wanted = expected
        .iter()
        .map(|(keys, recordings)| (keys[0].to_string(), recordings.len() as u32))
        .collect::<Vec<_>>();
    wanted.sort();
    assert_eq!(works, wanted);
    let candidates = super::query_work_recording_candidates(&conn, &all, &[]).unwrap();
    assert_eq!(candidates.len(), wanted.len());
    for candidate in &candidates {
        assert_eq!(
            candidate.recordings.len(),
            candidate.info.recordings as usize,
            "{}",
            candidate.info.key
        );
    }
}

// Prints, for a library database, the albums where a work has more than
// one recording, with the tracks of each, to check the rule of
// `recordings_of_works()` on a real library rather than on a copy of it:
//   DISMAS_LIBRARY_DB=~/.local/share/dismas/library.db \
//     cargo test print_work_splits -- --ignored --nocapture
#[test]
#[ignore]
fn print_work_splits() {
    let path = std::env::var_os("DISMAS_LIBRARY_DB").expect("DISMAS_LIBRARY_DB is not set");
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    super::add_functions(&conn).unwrap();
    let rows = super::query_work_rows(
        &conn,
        &super::TrackFilter::default(),
        &[],
        &[],
        super::Lengths::Skip,
    )
    .unwrap();
    let works = super::listed_works(&rows, &mut super::RecordingDates::new(&conn));
    let mut splits = 0;
    for work in &works {
        let mut by_album: std::collections::BTreeMap<&str, Vec<&[usize]>> =
            std::collections::BTreeMap::new();
        for recording in &work.recordings {
            let album = rows[recording.rows[0]].album_id.as_str();
            by_album.entry(album).or_default().push(&recording.rows);
        }
        for recordings in by_album.values().filter(|recordings| recordings.len() > 1) {
            splits += 1;
            let first = &rows[recordings[0][0]];
            println!(
                "{} on {}",
                work.info.name,
                first.album.as_deref().unwrap_or(&first.album_id)
            );
            for recording in recordings {
                let paths: Vec<String> = recording
                    .iter()
                    .map(|&i| rows[i].path.display().to_string())
                    .collect();
                println!("    {}", paths.join(", "));
            }
        }
    }
    println!(
        "{} works, {} recordings, {splits} albums with more than one recording of a work",
        works.len(),
        works
            .iter()
            .map(|work| work.recordings.len())
            .sum::<usize>()
    );
}

#[test]
fn albums_have_the_year_they_were_made_and_their_label() {
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::ExtraComments;

    // A reissue goes by its first release, and a recording date first
    let path = Path::new("/music/Cello Suites/01.flac");
    let mut tag = Tag::new(TagType::VorbisComments);
    tag.insert_text(ItemKey::RecordingDate, "2007-03-01".to_string());
    tag.insert_text(ItemKey::OriginalReleaseDate, "1961".to_string());
    tag.insert_text(ItemKey::Label, "Hyperion".to_string());
    tag.insert_text(ItemKey::CatalogNumber, "CDA67100".to_string());
    let tags = super::TrackTags::from_tag(
        &tag,
        path,
        Some(path),
        &ExtraComments::default(),
        (None, None),
    );
    let year = |tags: &super::TrackTags| tags.tag_dates.sort_dates().recorded.map(|date| date.year);
    assert_eq!(year(&tags), Some(1961));
    assert_eq!(tags.label.as_deref(), Some("Hyperion"));
    assert_eq!(tags.catalog_number.as_deref(), Some("CDA67100"));
    let extra = ExtraComments::with_recording_date("1960-05/1960-06");
    let tags = super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None));
    assert_eq!(year(&tags), Some(1960));

    // The album has the earliest year of its tracks, and its label
    let conn = test_db();
    for (path, year, label) in [
        ("/m/A/01", "1961-~~-~~", Some("Hyperion")),
        ("/m/A/02", "1960-05-~~", None),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                     recorded_sort_date, label, catalog_number)
                 VALUES (?1, ?1, 0, 0, 0, 0, 'a', 'Cello Suites', ?2, ?3, 'CDA67100')",
            rusqlite::params![path.as_bytes(), year, label],
        )
        .unwrap();
    }
    let albums = super::query_albums(&conn, &[]).unwrap();
    assert_eq!(albums.len(), 1);
    assert_eq!(albums[0].year.as_deref(), Some("1960"));
    assert_eq!(albums[0].label.as_deref(), Some("Hyperion"));
    assert_eq!(albums[0].catalog_number.as_deref(), Some("CDA67100"));
}

#[test]
fn albums_have_the_earliest_date_of_each_kind() {
    use lofty::tag::{ItemKey, Tag, TagType};

    use super::ExtraComments;

    // A reissue: the Date tag is the reissue's, and the dates are kept as
    // the album sorts compare them
    let path = Path::new("/music/Cello Suites/01.flac");
    let mut tag = Tag::new(TagType::VorbisComments);
    tag.insert_text(ItemKey::RecordingDate, "2007-3-1".to_string());
    tag.insert_text(ItemKey::OriginalReleaseDate, "1961".to_string());
    let extra = ExtraComments::with_recording_date("1960-05-02/1960-06-30");
    let tags = super::TrackTags::from_tag(&tag, path, Some(path), &extra, (None, None));
    let text = |date: Option<TagDate>| date.map(|date| date.to_string());
    let sort_dates = tags.tag_dates.sort_dates();
    assert_eq!(text(sort_dates.date).as_deref(), Some("2007-03-01"));
    assert_eq!(text(sort_dates.original).as_deref(), Some("1961"));
    assert_eq!(text(sort_dates.recorded).as_deref(), Some("1960-05-02"));

    // The album has the earliest of each, whichever tracks they come from,
    // in the order of the sorts: a date with only a year, here on the first
    // track, goes after the months of its year, here on the second
    let conn = test_db();
    for (path, date, original, recorded) in [
        ("/m/A/01", "2007-03-01", "1965", "1960-05-02"),
        ("/m/A/02", "2007-02", "1965-03", ""),
    ] {
        insert_dated_track(
            &conn,
            path,
            "a",
            TagDates::parse("", date, original, recorded),
        );
    }
    let albums = super::query_albums(&conn, &[]).unwrap();
    let dates = albums[0].dates;
    assert_eq!(text(dates.date).as_deref(), Some("2007-02"));
    assert_eq!(text(dates.original).as_deref(), Some("1965-03"));
    assert_eq!(text(dates.recorded).as_deref(), Some("1960-05-02"));
}

// A track of `album_id` with its dates as the tags have them
fn insert_dated_track(conn: &rusqlite::Connection, path: &str, album_id: &str, dates: TagDates) {
    let text = |date: Option<TagDate>| date.map(TagDate::sort_text);
    conn.execute(
        "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                             release_sort_date, date_tag_sort_date, original_sort_date,
                             recorded_sort_date)
         VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            path.as_bytes(),
            album_id,
            text(dates.release),
            text(dates.date),
            text(dates.original),
            text(dates.recorded),
        ],
    )
    .unwrap();
}

#[test]
fn an_album_goes_on_to_another_date_only_where_no_track_has_its_own() {
    // A reissue whose tracks have the reissue's Date tag and the original
    // date, but one track lacks the Date tag: it doesn't count under
    // Released with its original date, which would put the reissue among
    // the albums of 1965
    let conn = test_db();
    let dated = TagDates::parse("", "2015-04-28", "1965-01", "");
    let undated = TagDates::parse("", "", "1965-01", "");
    for (path, dates) in [("/m/A/01", dated), ("/m/A/02", undated), ("/m/A/03", dated)] {
        insert_dated_track(&conn, path, "a", dates);
    }
    // An album with a recording date on one track and nothing else: the
    // other sorts go on to it, as every track lacks their own
    insert_dated_track(&conn, "/m/B/01", "b", TagDates::default());
    insert_dated_track(
        &conn,
        "/m/B/02",
        "b",
        TagDates::parse("", "", "", "1960-05-02/1960-06-30"),
    );
    let albums = super::query_albums(&conn, &[]).unwrap();
    let text = |date: Option<TagDate>| date.map(|date| date.to_string());
    let dates = |id: &str| albums.iter().find(|album| album.id == id).unwrap().dates;
    let a = dates("a");
    assert_eq!(text(a.date).as_deref(), Some("2015-04-28"));
    assert_eq!(text(a.original).as_deref(), Some("1965-01"));
    assert_eq!(text(a.recorded).as_deref(), Some("1965-01"));
    let b = dates("b");
    assert_eq!(text(b.date).as_deref(), Some("1960-05-02"));
    assert_eq!(text(b.original).as_deref(), Some("1960-05-02"));
    assert_eq!(text(b.recorded).as_deref(), Some("1960-05-02"));

    // The SQL works the dates out as `TagDates::earliest()` does
    for (id, tracks) in [
        ("a", vec![dated, undated, dated]),
        (
            "b",
            vec![
                TagDates::default(),
                TagDates::parse("", "", "", "1960-05-02"),
            ],
        ),
    ] {
        let expected = [
            DateKind::Released,
            DateKind::OriginalRelease,
            DateKind::Recorded,
        ]
        .map(|kind| TagDates::earliest(&tracks, kind));
        let dates = dates(id);
        assert_eq!(
            [dates.date, dates.original, dates.recorded],
            expected,
            "{id}"
        );
    }
}

#[test]
fn the_recordings_of_a_work_are_in_the_order_they_were_recorded() {
    // Three albums of one year, whose titles are in the other order, and
    // one undated
    let conn = test_db();
    for (path, album, recorded) in [
        ("/m/A/01", "a", Some("1965")),
        ("/m/B/01", "b", Some("1965-11")),
        ("/m/C/01", "c", None),
        ("/m/D/01", "d", Some("1965-03-12")),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                 recorded_sort_date, work_key, work, work_composers)
             VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3, 'w', 'Work', 'Composer')",
            rusqlite::params![
                path.as_bytes(),
                album,
                recorded.and_then(TagDate::parse).map(TagDate::sort_text)
            ],
        )
        .unwrap();
    }
    let recordings = super::query_work_recordings(
        &conn,
        &["w".to_string()],
        &super::TrackFilter::default(),
        &[],
    )
    .unwrap();
    // As the Recorded sort of the albums: a date with only a year after
    // the months of its year
    let order: Vec<_> = recordings
        .iter()
        .map(|recording| (recording.album_id.as_str(), recording.year.as_deref()))
        .collect();
    assert_eq!(
        order,
        [
            ("d", Some("1965")),
            ("b", Some("1965")),
            ("a", Some("1965")),
            ("c", None)
        ]
    );
}

#[test]
fn a_recording_goes_on_to_another_date_only_where_no_track_has_its_own() {
    // A reissue of a recording made in 1965, with the original date on
    // every track and the recording date on all but one: the recording
    // was made in 1965, not in 1950, the original date the track without
    // a recording date would go on to
    let conn = test_db();
    for (path, original, recorded) in [
        ("/m/A/01", "1950", "1965-03"),
        ("/m/A/02", "1950", ""),
        ("/m/A/03", "1950", "1965-04"),
    ] {
        conn.execute(
            "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                 original_sort_date, recorded_sort_date, work_key, work,
                                 work_composers)
             VALUES (?1, ?1, 0, 0, 0, 0, 'a', 'a', ?2, ?3, 'w', 'Work', 'Composer')",
            rusqlite::params![
                path.as_bytes(),
                TagDate::parse(original).map(TagDate::sort_text),
                TagDate::parse(recorded).map(TagDate::sort_text),
            ],
        )
        .unwrap();
    }
    let recordings = super::query_work_recordings(
        &conn,
        &["w".to_string()],
        &super::TrackFilter::default(),
        &[],
    )
    .unwrap();
    assert_eq!(recordings.len(), 1);
    assert_eq!(recordings[0].year.as_deref(), Some("1965"));
}

#[test]
fn an_album_without_an_album_artist_is_by_most_of_its_tracks() {
    let conn = test_db();
    let add = |album: &str, n: usize, artist: Option<&str>| {
        for i in 0..n {
            let path = format!("/m/{album}/{}-{i}", artist.unwrap_or("none"));
            conn.execute(
                "INSERT INTO tracks (path, file, mtime, ctime, size, added, album_id, album,
                                         artist)
                     VALUES (?1, ?1, 0, 0, 0, 0, ?2, ?2, ?3)",
                rusqlite::params![path.as_bytes(), album, artist],
            )
            .unwrap();
        }
    };
    // A track "feat." another artist doesn't make a compilation
    add("feat", 9, Some("A"));
    add("feat", 1, Some("A feat. B"));
    // Five artists, none on most of the tracks
    for artist in ["A", "B", "C", "D", "E"] {
        add("compilation", 2, Some(artist));
    }
    // Half isn't most
    add("half", 2, Some("A"));
    add("half", 2, Some("B"));
    // One artist, and tracks without one
    add("one", 1, Some("A"));
    add("one", 3, None);
    add("none", 2, None);

    let artists: std::collections::HashMap<String, String> = super::query_albums(&conn, &[])
        .unwrap()
        .into_iter()
        .map(|album| (album.id, album.artist))
        .collect();
    assert_eq!(artists["feat"], "A");
    assert_eq!(artists["compilation"], "Various Artists");
    assert_eq!(artists["half"], "Various Artists");
    assert_eq!(artists["one"], "A");
    assert_eq!(artists["none"], "Unknown artist");
}

#[test]
fn mapped_paths_must_be_absolute() {
    for path in ["/music", "D:\\Music", "d:/Music", "\\\\nas\\music"] {
        assert!(super::is_mapped_path(path), "{path}");
    }
    for path in ["", "Music", "./Music", "\\Music", "/a\nb"] {
        assert!(!super::is_mapped_path(path), "{path:?}");
    }
}
