// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

// The catalogue numbers in the names of works, as in "BWV 846", "op. 2
// no. 4", "K. 331" or "Hob. XVI:52", by which the Works page puts the
// works of a composer in order. MusicBrainz keeps them only in the titles,
// so they're read from the names, as the Work Hierarchy plugin reads them
// (`_catalogue_numbers` in its hierarchy.py)

// The catalogues, as written before the numbers, lowercase and without a
// dot; a composer whose works are in two as often takes the first here
const CATALOGUES: [&str; 35] = [
    "anh", "b", "bb", "buxwv", "bwv", "cw", "d", "fp", "fwv", "gwv", "h", "hob", "hwv", "jw", "k",
    "kv", "l", "lwv", "m", "mwv", "op", "rv", "s", "sv", "swv", "sz", "trv", "twv", "w", "wab",
    "wd", "woo", "wq", "z", "zwv",
];
// The catalogues that number their groups in Roman numerals
const ROMAN_CATALOGUES: [&str; 2] = ["hob", "jw"];
// How a number in a set is introduced, as in "op. 2 no. 4"
const NUMBER_IN_SET: [&str; 4] = ["no", "nr", "nº", "n°"];
// How much of a name is read for its numbers. No real name comes near it,
// while a crafted tag can be up to lofty's 16 MiB, and the words and the
// numbers kept take about 28 bytes for each byte of a name, so three such
// names of one track took about 1 GB
const MAX_NAME_LEN: usize = 4096;

// A catalogue number of a work: its catalogue, and the parts of the number
// to put it in order by, e.g. 1006 and 1 for "BWV 1006.1/1006", or 384
// and "a" for "K. 384a/388", so that "BWV 1004" comes before "BWV 1007"
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogueNumber {
    // As in `CATALOGUES`, with "kv" as "k", as Köchel's catalogue is
    // written both ways
    pub catalogue: &'static str,
    pub order: Vec<(u32, String)>,
}

impl CatalogueNumber {
    // How the search keeps it, e.g. "bwv 1047", "op 2.4" or "hob 16.52":
    // the catalogue, then the parts of the number, which a search for a
    // number compares one by one, so that "op. 2" finds "op. 2 no. 4"
    pub fn search_text(&self) -> String {
        let parts: Vec<String> = self
            .order
            .iter()
            .map(|(value, rest)| format!("{value}{rest}"))
            .collect();
        format!("{} {}", self.catalogue, parts.join("."))
    }

    // As one word, e.g. "bwv1047", which plain words find, as they're
    // often typed without the space
    pub fn word(&self) -> Option<String> {
        let (value, rest) = self.order.first()?;
        Some(format!("{}{value}{rest}", self.catalogue))
    }
}

// A catalogue by the name written before its numbers, e.g. "BWV" or "op.",
// with "kv" as "k", as in `CatalogueNumber`
pub fn catalogue_name(word: &str) -> Option<&'static str> {
    let word = word.strip_suffix('.').unwrap_or(word).to_lowercase();
    CATALOGUES
        .iter()
        .copied()
        .find(|catalogue| *catalogue == word)
        .map(|catalogue| if catalogue == "kv" { "k" } else { catalogue })
}

// What the words of a name are split at
fn is_separator(c: char) -> bool {
    c.is_whitespace() || ",;()[]\"“”„".contains(c)
}

// The part of a name that's read for its numbers, and whether it's cut
// short: the name, or its start up to the last word that ends within
// `MAX_NAME_LEN`, as cutting inside a word would make "BWV 1004" "BWV 10"
fn read_part(name: &str) -> (&str, bool) {
    if name.len() <= MAX_NAME_LEN {
        return (name, false);
    }
    let start = &name[..name.floor_char_boundary(MAX_NAME_LEN)];
    let end = start.rfind(is_separator).unwrap_or(0);
    (&name[..end], true)
}

// The catalogue numbers in the name of a work, in the order they're in;
// none in "Book I" or "Act II"
pub fn catalogue_numbers(name: &str) -> Vec<CatalogueNumber> {
    let (name, cut) = read_part(name);
    let words: Vec<&str> = name
        .split(is_separator)
        .filter(|word| !word.is_empty())
        .collect();

    let is_number = |catalogue: &str, number: &str| {
        number.starts_with(|c: char| c.is_ascii_digit())
            || (ROMAN_CATALOGUES.contains(&catalogue) && number.starts_with(['I', 'V', 'X']))
    };

    let mut numbers = Vec::new();
    for (i, word) in words.iter().enumerate() {
        let following = words.get(i + 1).copied().unwrap_or_default();
        let (found, number, after) = match catalogue_name(word) {
            Some(found) if is_number(found, following) => (found, following, i + 2),
            _ => {
                // Written as one word, as in "op.2" or "W389"
                let split = word
                    .char_indices()
                    .find(|(_, c)| !c.is_alphabetic())
                    .map_or(word.len(), |(at, _)| at);
                let number = word[split..].strip_prefix('.').unwrap_or(&word[split..]);
                match catalogue_name(&word[..split]) {
                    Some(found) if is_number(found, number) => (found, number, i + 1),
                    _ => continue,
                }
            }
        };
        // A number that the words past the cut could go on with, as with
        // "op. 2 no." before a cut, is left out rather than read short
        if cut && after + 1 >= words.len() {
            continue;
        }
        let mut order = number_order(number);
        // A number in a set, as in "op. 2 no. 4", unless the name of a part
        // follows a colon, as in "op. 90, D. 899: No. 1"
        if !number.ends_with(':')
            && let [no, set_number, ..] = &words[after.min(words.len())..]
            && NUMBER_IN_SET.contains(&no.trim_end_matches('.').to_lowercase().as_str())
            && set_number.starts_with(|c: char| c.is_ascii_digit())
        {
            order.extend(number_order(set_number));
        }
        numbers.push(CatalogueNumber {
            catalogue: found,
            order,
        });
    }
    numbers
}

// The parts of a number to put it in order by: the number before a "/",
// which gives another one, cut at its dots and colons, each with its
// digits, or its Roman numeral, and what follows them
fn number_order(number: &str) -> Vec<(u32, String)> {
    let number = number.trim_end_matches(['.', ':']);
    let number = number.split('/').next().unwrap_or(number);
    number
        .split(['.', ':'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let digits = part.chars().take_while(char::is_ascii_digit).count();
            if digits > 0 {
                let value = part[..digits].parse::<u64>().unwrap_or(u64::MAX);
                let value = u32::try_from(value).unwrap_or(u32::MAX);
                (value, part[digits..].to_lowercase())
            } else if let Some(value) = roman_value(part) {
                (value, String::new())
            } else {
                (0, part.to_lowercase())
            }
        })
        .collect()
}

// The value of a Roman numeral, as in the groups of Hoboken's catalogue
fn roman_value(numeral: &str) -> Option<u32> {
    let value = |c: char| match c.to_ascii_uppercase() {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        _ => None,
    };
    let values = numeral.chars().map(value).collect::<Option<Vec<u32>>>()?;
    let total = values.iter().enumerate().fold(0i64, |total, (i, &v)| {
        if values.get(i + 1).is_some_and(|&next| next > v) {
            total - i64::from(v)
        } else {
            total + i64::from(v)
        }
    });
    u32::try_from(total).ok().filter(|&total| total > 0)
}

#[cfg(test)]
mod tests {
    use super::{CatalogueNumber, MAX_NAME_LEN, catalogue_numbers};

    fn number(catalogue: &'static str, order: &[(u32, &str)]) -> CatalogueNumber {
        CatalogueNumber {
            catalogue,
            order: order
                .iter()
                .map(|(value, rest)| (*value, rest.to_string()))
                .collect(),
        }
    }

    #[test]
    fn catalogue_numbers_are_read_as_the_plugin_reads_them() {
        for (name, numbers) in [
            (
                "Prelude and Fugue no. 1 in C major, BWV 846",
                vec![number("bwv", &[(846, "")])],
            ),
            (
                "Serenade no. 12 in C minor, K. 384a/388",
                vec![number("k", &[(384, "a")])],
            ),
            (
                "Eine kleine Nachtmusik, KV 525",
                vec![number("k", &[(525, "")])],
            ),
            (
                "Violin Partita No. 3 in E major, BWV 1006.1/1006",
                vec![number("bwv", &[(1006, ""), (1, "")])],
            ),
            (
                "Sonata no. 1 in F minor, op. 120 no. 1",
                vec![number("op", &[(120, ""), (1, "")])],
            ),
            (
                "Piano Sonata in E-flat major, Hob. XVI:52",
                vec![number("hob", &[(16, ""), (52, "")])],
            ),
            (
                "Concerto in C major, op. 8 no. 12, RV 178",
                vec![
                    number("op", &[(8, ""), (12, "")]),
                    number("rv", &[(178, "")]),
                ],
            ),
            ("Schwanengesang, D. 957", vec![number("d", &[(957, "")])]),
            (
                "Slavonic Dances, op. 46, B. 83",
                vec![number("op", &[(46, "")]), number("b", &[(83, "")])],
            ),
            (
                "Symphony no. 3 in E‐flat major, op. 55 “Eroica”",
                vec![number("op", &[(55, "")])],
            ),
            ("Messiah, HWV 56", vec![number("hwv", &[(56, "")])]),
            (
                "Sonata da camera, op.2 no. 4",
                vec![number("op", &[(2, ""), (4, "")])],
            ),
            // The name of a part after a colon isn't a number in a set
            (
                "Impromptus, op. 90, D. 899: No. 1",
                vec![number("op", &[(90, "")]), number("d", &[(899, "")])],
            ),
        ] {
            assert_eq!(catalogue_numbers(name), numbers, "{name}");
        }
    }

    #[test]
    fn names_without_numbers_have_none() {
        for name in [
            "Book I",
            "Act II",
            "Flow, my tears",
            "Missa Papae Marcelli",
            // A word of a name, not a catalogue
            "D minor",
            "",
        ] {
            assert_eq!(catalogue_numbers(name), [], "{name}");
        }
    }

    #[test]
    fn only_the_start_of_a_long_name_is_read() {
        let padding = "word ".repeat(MAX_NAME_LEN / 5 + 1);
        assert_eq!(catalogue_numbers(&format!("{padding}BWV 846")), []);
        assert_eq!(
            catalogue_numbers(&format!("BWV 846 {padding}BWV 847")),
            [number("bwv", &[(846, "")])]
        );
        // A name of one long word has nothing to cut it at
        assert_eq!(catalogue_numbers(&"x".repeat(MAX_NAME_LEN * 2)), []);
    }

    #[test]
    fn a_number_across_the_end_of_what_is_read_is_not_read_short() {
        // "BWV 10" before the end, "04" after it
        let name = format!("{} BWV 1004", "x".repeat(MAX_NAME_LEN - 7));
        assert_eq!(&name[MAX_NAME_LEN - 2..MAX_NAME_LEN], "10");
        assert_eq!(catalogue_numbers(&name), []);
        // "op. 2 no." before the end, the number in the set after it
        let name = format!("{} op. 2 no. 4", "x".repeat(MAX_NAME_LEN - 11));
        assert_eq!(&name[MAX_NAME_LEN - 4..MAX_NAME_LEN], "no. ");
        assert_eq!(catalogue_numbers(&name), []);
    }

    #[test]
    fn numbers_are_in_the_order_of_their_values() {
        let order = |name| catalogue_numbers(name).remove(0);
        assert!(order("BWV 1004") < order("BWV 1007"));
        assert!(order("BWV 846") < order("BWV 1001"));
        assert!(order("op. 2 no. 3") < order("op. 10 no. 1"));
        assert!(order("K. 384a") < order("K. 385"));
        assert!(order("Hob. II:1") < order("Hob. XVI:52"));
    }
}
