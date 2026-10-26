// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The tag of the Work Hierarchy plugin for MusicBrainz Picard, which
//! names the work a track performs and its movement

use super::*;

// A work in a path of the "WORK_HIERARCHY" tag, from the top-level work
// down to the one the track performs
struct PathWork<'a> {
    id: &'a str,
    // The full title, e.g. "Ouvertüre nach französischer Art h-Moll, BWV
    // 831: V. Passepied I"
    title: &'a str,
    // The title without the parent's, or a number matching the position,
    // e.g. "Passepied I"
    name: &'a str,
    // The number the plugin gives it: its position in the parent work, or
    // else the number its title starts with
    number: Option<u32>,
    // The number the plugin removed from the title to give the name, as the
    // title writes it, e.g. "V"
    numeral: Option<&'a str>,
    // The names of its composers; `None` if the plugin didn't record them
    composers: Option<Vec<&'a str>>,
    // For the work the track performs, when the track has only part of it,
    // the name the recording's title gives that part, e.g. "Ia. Overture –
    // Adagio" for the Ouverture of the Royal Fireworks, which MusicBrainz
    // links all the tracks of the overture to
    excerpt: Option<&'a str>,
}

impl PathWork<'_> {
    // The name to show, the part's if the track has only part of the work
    fn shown_name(&self) -> &str {
        self.excerpt.unwrap_or(self.name)
    }

    // Whether the name to show has a number of its own, which is the one
    // people know: "no. 22. Chorus" is the first number of Part II of
    // Messiah. A recording's title can number a part without a dot, as in
    // "3 Sarabande"
    fn has_own_number(&self) -> bool {
        let name = self.shown_name();
        is_numbered(name)
            || (self.excerpt.is_some() && name.starts_with(|c: char| c.is_ascii_digit()))
    }

    // The name to show with the number in front, unless it has its own
    fn numbered_name(&self) -> String {
        match self.number {
            Some(n) if !self.has_own_number() => format!("{n}. {}", self.shown_name()),
            _ => self.shown_name().to_string(),
        }
    }

    // The name of a level above the movement, with the number its title
    // gives it, e.g. "III. Sequentia"
    fn level_name(&self) -> String {
        match self.numeral {
            Some(numeral) => format!("{numeral}. {}", self.name),
            None => self.name.to_string(),
        }
    }
}

// The work and the movement to display, from the "WORK_HIERARCHY" tag
#[derive(Debug, PartialEq)]
pub(super) struct WorkLabels {
    // The name of the work, e.g. "Prelude and Fugue no. 1 in C major, BWV
    // 846"
    pub(super) work: String,
    // Its full title, with the works it's part of, e.g. "The Well-Tempered
    // Clavier, Book I: Prelude and Fugue no. 1 in C major, BWV 846"
    pub(super) title: String,
    // None if the track performs the whole work
    pub(super) movement: Option<MovementName>,
    // The composers of the work, or of the nearest work above it with
    // some; empty if none has any
    pub(super) composers: Vec<String>,
}

// The JSON of the "WORK_HIERARCHY" tag, written by the "Work Hierarchy"
// plugin of MusicBrainz Picard: a segment for each work the track performs,
// each with the path from the top-level work down to it. The fields the
// code below needs are optional, as tags from older versions of the plugin
// lack some
#[derive(Debug, Deserialize)]
pub(super) struct WorkHierarchy {
    version: u64,
    segments: Vec<HierarchySegment>,
}

#[derive(Debug, Deserialize)]
struct HierarchySegment {
    path: Vec<HierarchyWork>,
    // The work of the path that the plugin picked; tags from older
    // versions of the plugin don't have it
    #[serde(default, deserialize_with = "lenient_number")]
    work_index: Option<u64>,
    // The name the recording's title gives the part of the work the track
    // has, when it has only part of it
    excerpt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HierarchyWork {
    id: Option<String>,
    title: Option<String>,
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient_number")]
    number: Option<u64>,
    numeral: Option<String>,
    // `None` if the plugin didn't record them
    composers: Option<Vec<HierarchyComposer>>,
    // How MusicBrainz links the work to its parent, e.g. "act"
    #[serde(default)]
    part_attributes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HierarchyComposer {
    id: Option<String>,
    name: Option<String>,
}

// A number, or none if the field holds anything else, as one odd field
// shouldn't make the whole tag unreadable
fn lenient_number<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    Ok(serde_json::Value::deserialize(deserializer)?.as_u64())
}

impl HierarchyWork {
    // Whether MusicBrainz links the work to its parent as an act
    fn is_act(&self) -> bool {
        self.part_attributes
            .iter()
            .any(|attribute| attribute == "act")
    }
}

impl WorkHierarchy {
    // The tag, if it's a version this reads
    pub(super) fn parse(json: &str) -> Option<Self> {
        match serde_json::from_str::<Self>(json) {
            Ok(hierarchy) => (hierarchy.version == 1).then_some(hierarchy),
            Err(e) => {
                warn!("Invalid WORK_HIERARCHY tag: {e}");
                None
            }
        }
    }

    // The work and the movement to display. The plugin picks the work in
    // each path (`work_index`): the top-level one, or a part of it with a
    // catalogue number of its own, like each Prelude and Fugue of The
    // Well-Tempered Clavier. The movement names the levels below it. Returns
    // `None` if the tags should be used instead: if the track performs parts
    // of different works, or a top-level work without parts, which may still
    // be a movement in the tags if MusicBrainz didn't link it to its parent
    // work, or if an older version of the plugin wrote the tag, without the
    // work
    pub(super) fn labels(&self) -> Option<WorkLabels> {
        let paths = self
            .segments
            .iter()
            .map(|segment| {
                let mut path = segment
                    .path
                    .iter()
                    .map(|work| {
                        Some(PathWork {
                            id: work.id.as_deref()?,
                            title: work.title.as_deref()?,
                            name: work.name.as_deref()?,
                            number: work.number.and_then(|n| u32::try_from(n).ok()),
                            numeral: work.numeral.as_deref(),
                            composers: work.composers.as_ref().map(|composers| {
                                composers
                                    .iter()
                                    .filter_map(|composer| composer.name.as_deref())
                                    .collect()
                            }),
                            excerpt: None,
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                // An act of an opera, and the parts of an act, are numbered
                // in their names, e.g. "Acte I" and "No. 2 « Sur la place »",
                // as in the score. Their place in the work, which MusicBrainz
                // counts with every dialogue and recitative, e.g. "25.
                // Entr’acte", isn't a number anyone uses
                for (i, work) in path.iter_mut().enumerate() {
                    let parent = i.checked_sub(1).map(|parent| &segment.path[parent]);
                    if segment.path[i].is_act() || parent.is_some_and(HierarchyWork::is_act) {
                        work.number = None;
                    }
                }
                path.last_mut()?.excerpt = segment.excerpt.as_deref();
                let work_index = usize::try_from(segment.work_index?).ok()?;
                Some((path, work_index))
            })
            .collect::<Option<Vec<_>>>()?;

        let (first, depth) = paths.first()?;
        let work = first.get(*depth)?;
        // Parts of different works, e.g. the same movement in two catalogues
        if paths
            .iter()
            .any(|(path, index)| index != depth || path.get(*depth).is_none_or(|w| w.id != work.id))
        {
            return None;
        }

        let parts = paths
            .iter()
            .map(|(path, _)| &path[depth + 1..])
            .filter(|parts| !parts.is_empty())
            .collect::<Vec<_>>();
        // A track of a whole part of a work, e.g. a Prelude and Fugue in one
        // track, has no movement; a top-level work without parts is left to
        // the tags
        if parts.is_empty() && *depth == 0 {
            return None;
        }
        // MusicBrainz often links the composer to a work but not to its parts
        let composers = first[..=*depth]
            .iter()
            .rev()
            .filter_map(|work| work.composers.as_ref())
            .find(|composers| !composers.is_empty())
            .map(|composers| composers.iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        Some(WorkLabels {
            work: work.name.to_string(),
            title: work.title.to_string(),
            movement: (!parts.is_empty()).then(|| movement_name(&parts)),
            composers,
        })
    }

    // The ids of the composers of the works a track performs, the last in
    // each path; `None` if the plugin that wrote the tag didn't record them.
    // MusicBrainz often links the composer to a work but not to its parts,
    // so a work without composers takes those of its nearest parent that has
    // some, as the plugin does for the composer tags
    pub(super) fn composer_ids(&self) -> Option<Vec<String>> {
        let mut ids: Vec<String> = Vec::new();
        for segment in &self.segments {
            let (last, parents) = segment.path.split_last()?;
            let mut composers = last.composers.as_ref()?;
            if composers.is_empty() {
                // Parents that weren't looked up have no list
                if let Some(parent) = parents
                    .iter()
                    .rev()
                    .filter_map(|work| work.composers.as_ref())
                    .find(|composers| !composers.is_empty())
                {
                    composers = parent;
                }
            }
            for composer in composers {
                let id = composer.id.as_deref().filter(|id| is_mbid(id))?;
                if !ids.iter().any(|i| i == id) {
                    ids.push(id.to_string());
                }
            }
        }
        Some(ids)
    }
}

// The name of a movement below the top-level work: the levels above it,
// if any, e.g. "Part II", and the movement, e.g. "no. 22. Chorus"
#[derive(Debug, PartialEq)]
pub(super) struct MovementName {
    pub(super) levels: Option<String>,
    pub(super) movement: String,
}

impl MovementName {
    pub(super) fn full(&self) -> String {
        match &self.levels {
            Some(levels) => format!("{levels}: {}", self.movement),
            None => self.movement.clone(),
        }
    }
}

// The name of the movements, from the levels below the top-level work of
// each; the levels they share are named once, and the movements of the
// same part are numbered together, e.g. "Atto IV: 1, 4. “Era più calmo?”
// / “Ave Maria”"
fn movement_name(parts: &[&[PathWork]]) -> MovementName {
    let first = parts[0];
    let shared = (0..first.len() - 1)
        .take_while(|&i| {
            parts
                .iter()
                .all(|p| i + 1 < p.len() && p[i].id == first[i].id)
        })
        .count();
    let rest = parts.iter().map(|p| &p[shared..]).collect::<Vec<_>>();

    let movements = if rest.iter().all(|p| p.len() == 1) {
        let works = rest.iter().map(|p| &p[0]).collect::<Vec<_>>();
        let names = works.iter().map(|w| w.shown_name()).join(" / ");
        let numbers = works.iter().map(|w| w.number).collect::<Option<Vec<_>>>();
        match numbers {
            Some(n) if !works.iter().any(|w| w.has_own_number()) => {
                format!("{}. {names}", number_ranges(&n))
            }
            _ => names,
        }
    } else {
        rest.iter()
            .map(|p| {
                let (last, above) = p.split_last().expect("parts are not empty");
                above
                    .iter()
                    .map(PathWork::level_name)
                    .chain([last.numbered_name()])
                    .join(": ")
            })
            .join(" / ")
    };

    MovementName {
        levels: (shared > 0).then(|| first[..shared].iter().map(PathWork::level_name).join(": ")),
        movement: movements,
    }
}

// Whether a name starts with a number, e.g. "no. 22. Chorus", "Nr. 3.
// Tragödie", "LXVII. Diffusa est", "1a. Introduction", "12a Recitativo",
// "IIIa. [Allegro]", "16: Minuet" or "b. Benedictus"
fn is_numbered(name: &str) -> bool {
    let lower = name.to_lowercase();
    let after_no = ["no.", "no ", "nr.", "nr ", "nº", "n°", "№"]
        .iter()
        .find_map(|prefix| lower.strip_prefix(prefix));
    if let Some(rest) = after_no {
        return rest.trim_start().starts_with(|c: char| c.is_ascii_digit());
    }

    let end = name
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(name.len());
    let (token, after) = name.split_at(end);
    // A number can have a letter for a part of it, e.g. "12a" or "IIIa"
    let number = token
        .strip_suffix(|c: char| c.is_ascii_lowercase())
        .unwrap_or(token);
    let part = number.len() < token.len();
    if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
        // Without a dot only with a letter, as "2 Waltzes" is a count
        return after.starts_with(['.', ')', ':']) || (part && after.starts_with(' '));
    }
    let roman = !number.is_empty() && number.chars().all(|c| "IVXLC".contains(c));
    let letter = token.len() == 1 && token.chars().all(|c| c.is_ascii_lowercase());
    // A range of parts, e.g. "IIa-b." for the bars of a movement
    let after = match after.strip_prefix(['-', '–']) {
        Some(rest) if part && roman => rest.trim_start_matches(|c: char| c.is_ascii_lowercase()),
        _ => after,
    };
    (roman || letter) && after.starts_with(['.', ')'])
}

// The numbers as a list where consecutive ones are joined into a range,
// e.g. "5–6" or "1–3, 5"
fn number_ranges(numbers: &[u32]) -> String {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for &n in numbers {
        match ranges.last_mut() {
            Some((_, end)) if end.checked_add(1) == Some(n) => *end = n,
            _ => ranges.push((n, n)),
        }
    }

    ranges
        .iter()
        .map(|&(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}–{end}")
            }
        })
        .join(", ")
}

#[cfg(test)]
mod tests {
    use lofty::tag::TagType;

    use super::*;

    fn work_hierarchy_labels(json: &str) -> Option<WorkLabels> {
        WorkHierarchy::parse(json)?.labels()
    }

    fn work_hierarchy_composer_ids(json: &str) -> Option<Vec<String>> {
        WorkHierarchy::parse(json)?.composer_ids()
    }

    type Work<'a> = (&'a str, &'a str, Option<u32>);

    // A "WORK_HIERARCHY" tag with a segment for each path, given as the
    // id, name and number of each work, the work being the one at
    // `work_index` in each path
    fn hierarchy_at(paths: &[&[Work]], work_index: usize) -> String {
        let segments = paths
            .iter()
            .map(|path| {
                let path = path
                    .iter()
                    .map(|(id, name, number)| {
                        serde_json::json!({
                            "id": id, "title": name, "name": name, "number": number,
                        })
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({ "path": path, "work_index": work_index, "attributes": [] })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "version": 1, "segments": segments }).to_string()
    }

    fn hierarchy(paths: &[&[Work]]) -> String {
        hierarchy_at(paths, 0)
    }

    fn labels_at(paths: &[&[Work]], work_index: usize) -> Option<(String, String)> {
        work_hierarchy_labels(&hierarchy_at(paths, work_index))
            .map(|l| (l.work, l.movement.map_or_else(String::new, |m| m.full())))
    }

    fn labels(paths: &[&[Work]]) -> Option<(String, String)> {
        labels_at(paths, 0)
    }

    fn movement(paths: &[&[Work]]) -> Option<String> {
        labels(paths).map(|(_, movement)| movement)
    }

    // The movement of a "WORK_HIERARCHY" tag
    fn movement_of(json: &serde_json::Value) -> Option<String> {
        work_hierarchy_labels(&json.to_string())
            .and_then(|l| l.movement)
            .map(|m| m.full())
    }

    const SUITE: Work = ("bwv831", "Ouvertüre, BWV 831", None);

    const MESSIAH: Work = ("messiah", "Messiah, HWV 56", None);

    #[test]
    fn work_hierarchy_numbers_the_movements() {
        assert_eq!(
            labels(&[&[SUITE, ("v", "Passepied I", Some(5))]]),
            Some((
                "Ouvertüre, BWV 831".to_string(),
                "5. Passepied I".to_string()
            ))
        );
        assert_eq!(
            movement(&[
                &[SUITE, ("v", "Passepied I", Some(5))],
                &[SUITE, ("vi", "Passepied II", Some(6))],
            ])
            .as_deref(),
            Some("5–6. Passepied I / Passepied II")
        );
    }

    #[test]
    fn work_hierarchy_numbers_the_levels_as_their_titles_do() {
        let json = serde_json::json!({ "version": 1, "segments": [{ "work_index": 0, "path": [
            { "id": "r", "title": "Requiem, op. 89", "name": "Requiem, op. 89", "number": null },
            { "id": "1", "title": "Requiem, op. 89: I. oddíl", "name": "oddíl",
              "number": 1, "numeral": "I" },
            { "id": "3", "title": "…: III. Sequentia", "name": "Sequentia",
              "number": 3, "numeral": "III" },
            { "id": "q", "title": "…: Quid sum miser", "name": "Quid sum miser", "number": 3 },
        ]}]});
        assert_eq!(
            movement_of(&json).as_deref(),
            Some("I. oddíl: III. Sequentia: 3. Quid sum miser")
        );
    }

    #[test]
    fn work_hierarchy_keeps_the_numbers_of_parts_of_movements() {
        // The third movement of the suite, in three tracks
        let json = serde_json::json!({ "version": 1, "segments": [{ "work_index": 1, "path": [
            { "id": "w", "title": "Water Music, HWV 348–350", "name": "Water Music, HWV 348–350",
              "number": null },
            { "id": "s", "title": "Water Music Suite no. 1 in F major, HWV 348",
              "name": "Water Music Suite no. 1 in F major, HWV 348", "number": 1 },
            { "id": "3", "title": "…: III. Allegro - Andante - Allegro da capo",
              "name": "Allegro - Andante - Allegro da capo", "number": 3, "numeral": "III" },
            { "id": "3a", "title": "…: IIIa. [Allegro]", "name": "IIIa. [Allegro]", "number": 1 },
        ]}]});
        assert_eq!(
            movement_of(&json).as_deref(),
            Some("III. Allegro - Andante - Allegro da capo: IIIa. [Allegro]")
        );
    }

    #[test]
    fn a_part_of_a_movement_is_named_after_its_recording() {
        let movement = |excerpt: Option<&str>| {
            movement_of(&serde_json::json!({ "version": 1, "segments": [{
                "work_index": 0, "attributes": ["partial"], "excerpt": excerpt, "path": [
                    { "id": "hwv351", "title": "Music for the Royal Fireworks, HWV 351",
                      "name": "Music for the Royal Fireworks, HWV 351", "number": null },
                    { "id": "i", "title": "Music for the Royal Fireworks, HWV 351: I. Ouverture",
                      "name": "Ouverture", "number": 1, "numeral": "I" },
                ],
            }]}))
        };
        // An overture in two tracks
        assert_eq!(
            movement(Some("Ia. Overture – Adagio")).as_deref(),
            Some("Ia. Overture – Adagio")
        );
        assert_eq!(
            movement(Some("IIa. Gloria")).as_deref(),
            Some("IIa. Gloria")
        );
        // The number of the movement goes before a part without one
        assert_eq!(movement(Some("Adagio")).as_deref(), Some("1. Adagio"));
        assert_eq!(
            movement(Some("3 Sarabande")).as_deref(),
            Some("3 Sarabande")
        );
        // When the recording's title doesn't name the part
        assert_eq!(movement(None).as_deref(), Some("1. Ouverture"));

        // A number of an opera, in its act
        let json = serde_json::json!({ "version": 1, "segments": [{
            "work_index": 0, "attributes": ["partial"],
            "excerpt": "Air « Prendre le dessin d’un bijou » (Gerald)", "path": [
                { "id": "lakme", "title": "Lakmé", "name": "Lakmé", "number": null },
                { "id": "act1", "title": "Lakmé: Acte I", "name": "Acte I", "number": 1 },
                { "id": "air", "title": "…", "name": "Air « Prendre le dessin d’un bijou »",
                  "number": 8 },
            ],
        }]});
        assert_eq!(
            movement_of(&json).as_deref(),
            Some("Acte I: 8. Air « Prendre le dessin d’un bijou » (Gerald)")
        );
    }

    // A part of an act of an opera, as the plugin writes it: the act and
    // the part both have the place MusicBrainz gives them
    fn opera_part(name: &str, number: u32) -> serde_json::Value {
        serde_json::json!({ "version": 1, "segments": [{
            "path": [
                { "id": "carmen", "title": "Carmen, WD 31", "name": "Carmen, WD 31",
                  "number": null, "part_attributes": [] },
                { "id": "act1", "title": "Carmen, WD 31: Acte I", "name": "Acte I",
                  "number": 2, "part_attributes": ["act"] },
                { "id": "part", "title": name, "name": name, "number": number,
                  "part_attributes": ["number"] },
            ],
            "work_index": 0,
            "attributes": [],
        }]})
    }

    #[test]
    fn the_parts_of_an_act_have_no_place_number() {
        // Numbered in the score, and in their names
        assert_eq!(
            movement_of(&opera_part("No. 2 « Sur la place »", 1)).as_deref(),
            Some("Acte I: No. 2 « Sur la place »")
        );
        // Not numbered in the score: their place in the act would look
        // like a number of the score, e.g. "25. Entr’acte" after No. 11
        assert_eq!(
            movement_of(&opera_part("Entr’acte", 25)).as_deref(),
            Some("Acte I: Entr’acte")
        );
        assert_eq!(
            movement_of(&opera_part("Dialogue « Regardez donc cette petite »", 2)).as_deref(),
            Some("Acte I: Dialogue « Regardez donc cette petite »")
        );

        // A track linked to the whole act is named after it, without its
        // place in the opera
        let mut whole_act = opera_part("", 0);
        whole_act["segments"][0]["path"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert_eq!(movement_of(&whole_act).as_deref(), Some("Acte I"));
    }

    #[test]
    fn work_hierarchy_numbers_the_lowest_level() {
        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = ("pf12", "Prelude and Fugue no. 12 in F minor", Some(12));
        assert_eq!(
            movement(&[&[wtc, book, pf, ("p", "Prelude", Some(1))]]).as_deref(),
            Some("Book I: Prelude and Fugue no. 12 in F minor: 1. Prelude")
        );

        // The levels in common are only named once
        let otello = ("otello", "Otello", None);
        let act = ("act4", "Atto IV", Some(5));
        assert_eq!(
            movement(&[
                &[otello, act, ("a", "“Era più calmo?”", Some(1))],
                &[otello, act, ("b", "“Ave Maria”", Some(4))],
            ])
            .as_deref(),
            Some("Atto IV: 1, 4. “Era più calmo?” / “Ave Maria”")
        );
        let opera = ("opera", "Opera", None);
        assert_eq!(
            movement(&[
                &[opera, ("act1", "Act I", Some(1)), ("f", "Finale", Some(9))],
                &[
                    opera,
                    ("act2", "Act II", Some(2)),
                    ("i", "Introduction", Some(1))
                ],
            ])
            .as_deref(),
            Some("Act I: 9. Finale / Act II: 1. Introduction")
        );
    }

    #[test]
    fn work_hierarchy_separates_the_levels_above_the_movement() {
        let name = |paths: &[&[Work]]| {
            work_hierarchy_labels(&hierarchy(paths))
                .unwrap()
                .movement
                .unwrap()
        };
        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = ("pf1", "Prelude and Fugue no. 1", Some(1));
        assert_eq!(
            name(&[&[wtc, book, pf, ("p", "Prelude", Some(1))]]),
            MovementName {
                levels: Some("Book I: Prelude and Fugue no. 1".to_string()),
                movement: "1. Prelude".to_string(),
            }
        );
        assert_eq!(
            name(&[&[SUITE, ("v", "Passepied I", Some(5))]]),
            MovementName {
                levels: None,
                movement: "5. Passepied I".to_string(),
            }
        );
        // Only the levels in common are above the movements
        let opera = ("opera", "Opera", None);
        assert_eq!(
            name(&[
                &[opera, ("act1", "Act I", Some(1)), ("f", "Finale", Some(9))],
                &[
                    opera,
                    ("act2", "Act II", Some(2)),
                    ("i", "Introduction", Some(1))
                ],
            ]),
            MovementName {
                levels: None,
                movement: "Act I: 9. Finale / Act II: 1. Introduction".to_string(),
            }
        );
    }

    #[test]
    fn work_hierarchy_keeps_the_numbers_in_names() {
        let part = ("part2", "Part II", Some(2));
        let chorus = ("22", "no. 22. Chorus “Behold the Lamb of God”", Some(1));
        assert_eq!(
            movement(&[&[MESSIAH, part, chorus]]).as_deref(),
            Some("Part II: no. 22. Chorus “Behold the Lamb of God”")
        );
        // Without a number, there is none
        assert_eq!(
            movement(&[&[MESSIAH, part, ("x", "Chorus", None)]]).as_deref(),
            Some("Part II: Chorus")
        );
    }

    #[test]
    fn work_hierarchy_is_ignored_when_it_does_not_fit() {
        // Parts of different works, e.g. the same movement in two
        // catalogues
        assert_eq!(
            labels(&[
                &[("t1", "Mass, T 1", None), ("a", "Benedictus", Some(5))],
                &[("p31", "Mass, P. 31", None), ("b", "Benedictus", Some(4))],
            ]),
            None
        );
        // A work without parts, as the tags may know its parent work
        assert_eq!(labels(&[&[("song", "Song", None)]]), None);
        // A tag from a version of the plugin that didn't pick the work
        let json = r#"{"version":1,"segments":[{"path":[
            {"id":"s","title":"Suite","name":"Suite","position":null},
            {"id":"p","title":"Suite: I. Prélude","name":"Prélude","position":1}
        ]}]}"#;
        assert_eq!(work_hierarchy_labels(json), None);
        assert_eq!(work_hierarchy_labels("not json"), None);
        assert_eq!(
            work_hierarchy_labels(r#"{"version":2,"segments":[]}"#),
            None
        );
        assert_eq!(
            work_hierarchy_labels(r#"{"version":1,"segments":[]}"#),
            None
        );
    }

    #[test]
    fn the_work_is_the_one_the_plugin_picks() {
        // A Prelude and Fugue of The Well-Tempered Clavier, which has a
        // catalogue number of its own
        let json = serde_json::json!({ "version": 1, "segments": [{ "work_index": 2, "path": [
            { "id": "wtc", "title": "The Well-Tempered Clavier",
              "name": "The Well-Tempered Clavier", "number": null },
            { "id": "book1", "title": "The Well-Tempered Clavier, Book I",
              "name": "Book I", "number": 1 },
            { "id": "bwv846",
              "title": "The Well-Tempered Clavier, Book I: Prelude and Fugue no. 1 in C major, BWV 846",
              "name": "Prelude and Fugue no. 1 in C major, BWV 846", "number": 1 },
            { "id": "f", "title": "…: Fugue", "name": "Fugue", "number": 2 },
        ]}]});
        assert_eq!(
            work_hierarchy_labels(&json.to_string()),
            Some(WorkLabels {
                work: "Prelude and Fugue no. 1 in C major, BWV 846".to_string(),
                title:
                    "The Well-Tempered Clavier, Book I: Prelude and Fugue no. 1 in C major, BWV 846"
                        .to_string(),
                movement: Some(MovementName {
                    levels: None,
                    movement: "2. Fugue".to_string(),
                }),
                composers: Vec::new(),
            })
        );

        let wtc = ("wtc", "The Well-Tempered Clavier", None);
        let book = ("book1", "Book I", Some(1));
        let pf = (
            "bwv846",
            "Prelude and Fugue no. 1 in C major, BWV 846",
            Some(1),
        );
        let prelude = ("p", "Prelude", Some(1));
        let fugue = ("f", "Fugue", Some(2));
        // Both movements of it
        assert_eq!(
            labels_at(&[&[wtc, book, pf, prelude], &[wtc, book, pf, fugue]], 2),
            Some((
                "Prelude and Fugue no. 1 in C major, BWV 846".to_string(),
                "1–2. Prelude / Fugue".to_string()
            ))
        );
        // A whole Prelude and Fugue in one track is a work, not a movement
        assert_eq!(
            labels_at(&[&[wtc, book, pf]], 2),
            Some((
                "Prelude and Fugue no. 1 in C major, BWV 846".to_string(),
                String::new()
            ))
        );
        // Movements of two of them, which are only parts of the same
        // collection
        let pf2 = (
            "bwv847",
            "Prelude and Fugue no. 2 in C minor, BWV 847",
            Some(2),
        );
        assert_eq!(
            labels(&[&[wtc, book, pf, fugue], &[wtc, book, pf2, prelude]]),
            Some((
                "The Well-Tempered Clavier".to_string(),
                "Book I: Prelude and Fugue no. 1 in C major, BWV 846: 2. Fugue / Prelude and Fugue no. 2 in C minor, BWV 847: 1. Prelude"
                    .to_string()
            ))
        );
    }

    #[test]
    fn names_with_their_own_numbers() {
        for name in [
            "no. 22. Chorus",
            "No. 9 Interlude",
            "Nr. 3. Der arme Peter",
            "28. Recitativo",
            "1a. Introduction",
            "16: (Minuet)",
            "LXVII. Diffusa est gratia",
            "b. Benedictus",
            "IV) Finale",
            // Parts of a number
            "IIIa. [Allegro]",
            "XXVIIa. Arie (Sopran, Alt) und Chor",
            // No. 50a of the St Matthew Passion
            "La. Recitativo (Evangelista)",
            "12a Recitativo \"Und Hannas sandte ihn gebunden\"",
            "IIa-b. Andante quasi Allegretto [Bars 1-50]",
        ] {
            assert!(is_numbered(name), "{name}");
        }
        for name in [
            "Prelude",
            "Nocturne",
            "No vale la pena",
            "1812 Overture",
            "Credo",
            "Vivace",
            "I lacci tende più forti",
            "Scena I. (no. 1) Duetto",
            "D. 960",
            "2 Waltzes, op. 54",
            "Il Contento",
            "La Balançoire",
            "In der Ferne",
        ] {
            assert!(!is_numbered(name), "{name}");
        }
    }

    #[test]
    fn number_ranges_join_consecutive_numbers() {
        assert_eq!(number_ranges(&[5]), "5");
        assert_eq!(number_ranges(&[5, 6]), "5–6");
        assert_eq!(number_ranges(&[1, 2, 3, 5]), "1–3, 5");
        assert_eq!(number_ranges(&[6, 5]), "6, 5");
        // The largest number a tag can give, followed by another
        assert_eq!(number_ranges(&[u32::MAX, 1]), format!("{}, 1", u32::MAX));
    }

    #[test]
    fn composer_ids_are_read_from_the_performed_works() {
        let json = serde_json::json!({
            "version": 1,
            "segments": [
                { "path": [
                    { "id": "w1", "composers": [{ "id": "aaaa", "name": "A" }] },
                    { "id": "w2", "composers": [{ "id": "bbbb", "name": "B" }] },
                ] },
                { "path": [
                    { "id": "w3", "composers": [
                        { "id": "bbbb", "name": "B" }, { "id": "cccc", "name": "C" },
                    ] },
                ] },
            ],
        });
        assert_eq!(
            work_hierarchy_composer_ids(&json.to_string()),
            Some(vec!["bbbb".to_string(), "cccc".to_string()])
        );

        // Tags written before the plugin recorded composers
        let json = r#"{"version":1,"segments":[{"path":[{"id":"w1"}]}]}"#;
        assert_eq!(work_hierarchy_composer_ids(json), None);
        // Or by a lookup that failed
        let json = r#"{"version":1,"segments":[{"path":[{"id":"w1","composers":null}]}]}"#;
        assert_eq!(work_hierarchy_composer_ids(json), None);
    }

    #[test]
    fn composer_ids_are_inherited_from_the_nearest_parent() {
        let json = serde_json::json!({
            "version": 1,
            "segments": [
                { "path": [
                    { "id": "opera", "composers": [{ "id": "aaaa" }] },
                    { "id": "act", "composers": [{ "id": "bbbb" }] },
                    { "id": "scene", "composers": null },
                    { "id": "number", "composers": [] },
                ] },
                // A work with its own composers keeps them
                { "path": [
                    { "id": "suite", "composers": [{ "id": "aaaa" }] },
                    { "id": "dance", "composers": [{ "id": "cccc" }] },
                ] },
                // And one with none anywhere adds nothing
                { "path": [{ "id": "song", "composers": [] }] },
            ],
        });
        assert_eq!(
            work_hierarchy_composer_ids(&json.to_string()),
            Some(vec!["bbbb".to_string(), "cccc".to_string()])
        );
    }

    #[test]
    fn work_hierarchy_replaces_the_work_tags() {
        // Whatever the work and movement tags say
        let mut tag = Tag::new(TagType::VorbisComments);
        tag.insert_text(ItemKey::Work, "Messiah, HWV 56: Part II".to_string());
        tag.insert_text(ItemKey::Movement, "Chorus".to_string());
        tag.insert_text(ItemKey::MovementNumber, "1".to_string());
        let part = ("part2", "Part II", Some(2));
        let chorus = ("22", "no. 22. Chorus", Some(1));
        let extra = ExtraComments {
            work_hierarchy: Some(hierarchy(&[&[MESSIAH, part, chorus]])),
            ..Default::default()
        };
        let credits = TrackCredits::from_tag(&tag, &extra);
        assert_eq!(credits.work.as_deref(), Some("Messiah, HWV 56"));
        assert_eq!(
            credits.movement_label().as_deref(),
            Some("Part II: no. 22. Chorus")
        );
    }

    #[test]
    fn work_hierarchy_is_read_from_ape_mp3_and_mp4_tags() {
        use lofty::{
            ape::{ApeItem, ApeTag},
            id3::v2::Id3v2Tag,
            mp4::{Atom, AtomData, AtomIdent, Ilst},
            tag::ItemValue,
        };

        let json = r#"{"version":1,"segments":[]}"#;
        // As MusicBrainz Picard writes the tags it has no field for
        let mut ape = ApeTag::new();
        for (key, value) in [("Work_Hierarchy", json), ("Cuesheet", "FILE")] {
            ape.insert(ApeItem::new(key.to_string(), ItemValue::Text(value.to_string())).unwrap());
        }
        let extra = ExtraComments::from_ape(Some(&ape));
        assert_eq!(extra.work_hierarchy.as_deref(), Some(json));
        assert_eq!(extra.cue_sheet.as_deref(), Some("FILE"));
        let mut id3v2 = Id3v2Tag::new();
        let _ = id3v2.insert_user_text("work_hierarchy".to_string(), json.to_string());
        assert_eq!(
            ExtraComments::from_id3v2(Some(&id3v2))
                .work_hierarchy
                .as_deref(),
            Some(json)
        );
        let mut ilst = Ilst::new();
        ilst.insert(Atom::new(
            AtomIdent::Freeform {
                mean: "com.apple.iTunes".into(),
                name: "work_hierarchy".into(),
            },
            AtomData::UTF8(json.to_string()),
        ));
        assert_eq!(
            ExtraComments::from_mp4(Some(&ilst))
                .work_hierarchy
                .as_deref(),
            Some(json)
        );

        assert_eq!(
            ExtraComments::from_id3v2(Some(&Id3v2Tag::new())).work_hierarchy,
            None
        );
        assert_eq!(ExtraComments::from_mp4(None).work_hierarchy, None);
        assert_eq!(ExtraComments::from_ape(None).work_hierarchy, None);
    }

    #[test]
    fn work_composers_are_those_of_the_work_not_the_movement() {
        let work = |id: &str, name: &str, composers: serde_json::Value| serde_json::json!({ "id": id, "title": name, "name": name, "composers": composers });
        // The composers of the work at `work_index`
        let composers = |work_index: usize, path: Vec<serde_json::Value>| {
            let json = serde_json::json!({ "version": 1, "segments": [
                { "path": path, "work_index": work_index },
            ] });
            work_hierarchy_labels(&json.to_string()).map(|labels| labels.composers)
        };
        let vivaldi = serde_json::json!([{ "id": "aaaa", "name": "Antonio Vivaldi" }]);

        // MusicBrainz credits Bottesini on two movements of RV 531, which
        // would split the work in the queue
        assert_eq!(
            composers(
                0,
                vec![
                    work(
                        "rv531",
                        "Concerto for Two Cellos in G minor, RV 531",
                        vivaldi
                    ),
                    work(
                        "largo",
                        "Largo",
                        serde_json::json!([
                            { "id": "bbbb", "name": "Giovanni Bottesini" },
                            { "id": "aaaa", "name": "Antonio Vivaldi" },
                        ]),
                    ),
                ]
            ),
            Some(vec!["Antonio Vivaldi".to_string()])
        );

        // A part of a collection that is the work has its own, or else
        // those of the nearest work above it with some
        let bach = serde_json::json!([{ "id": "cccc", "name": "Johann Sebastian Bach" }]);
        let wtc = |pf_composers| {
            composers(
                2,
                vec![
                    work("wtc", "The Well-Tempered Clavier", bach.clone()),
                    work("book1", "Book I", serde_json::json!([])),
                    work(
                        "bwv846",
                        "Prelude and Fugue no. 1 in C major, BWV 846",
                        pf_composers,
                    ),
                    work("f", "Fugue", serde_json::Value::Null),
                ],
            )
        };
        let anon = serde_json::json!([{ "id": "dddd", "name": "Anonymous" }]);
        assert_eq!(wtc(anon), Some(vec!["Anonymous".to_string()]));
        assert_eq!(
            wtc(serde_json::Value::Null),
            Some(vec!["Johann Sebastian Bach".to_string()])
        );

        // And none if no work has any
        assert_eq!(
            composers(
                0,
                vec![
                    work("fvb", "Fitzwilliam Virginal Book", serde_json::json!([])),
                    work("walsingham", "Walsingham", serde_json::Value::Null),
                ]
            ),
            Some(Vec::new())
        );
    }
}
