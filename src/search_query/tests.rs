// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

use super::{values::period, *};

fn search(text: &str) -> Query {
    Query::parse(text, Mode::Search)
}

fn rule(text: &str) -> Query {
    Query::parse(text, Mode::Rule)
}

fn word(text: &str) -> Node {
    Node::Word(text.to_string())
}

fn kinds(query: &Query) -> Vec<ProblemKind> {
    query.problems().iter().map(|p| p.kind.clone()).collect()
}

// A track with the text of its tags and its fields, as `code\tvalue` lines
fn track<'a>(search_key: &'a str, fields: &'a str) -> Candidate<'a> {
    Candidate {
        search_key,
        fields,
        ..Default::default()
    }
}

const NOW: Context = Context { now: 1_793_448_000 };

fn finds(query: &str, candidate: &Candidate) -> bool {
    search(query).matches(candidate, &NOW)
}

const BACH: &str = "co\tjohann sebastian bach\nco\tbach, johann sebastian\nyr\t1985\n\
                    pf\treinhard goebel (violin)\nfm\tcantata\nca\tbwv 1047\nln\t301\n\
                    re\t1986-11\nis\tlive\nge\tbaroque\n";

#[test]
fn and_binds_tighter_than_or() {
    assert_eq!(
        search("a OR b c").root,
        Node::Or(vec![word("a"), Node::And(vec![word("b"), word("c")])])
    );
    assert_eq!(
        search("(a OR b) c").root,
        Node::And(vec![Node::Or(vec![word("a"), word("b")]), word("c")])
    );
    assert_eq!(
        search("a AND b").root,
        Node::And(vec![word("a"), word("b")])
    );
    assert_eq!(
        search("NOT a -b").root,
        Node::And(vec![
            Node::Not(Box::new(word("a"))),
            Node::Not(Box::new(word("b")))
        ])
    );
    // Two "-" cancel out
    assert_eq!(search("--a").root, word("a"));
}

#[test]
fn what_doesnt_parse_is_ignored_in_a_search() {
    for text in ["a )", "a -", "a OR", "OR a", "a AND", "AND a", "(a", "a ( "] {
        assert_eq!(search(text).root, word("a"), "{text}");
        assert!(!search(text).has_errors(), "{text}");
        assert!(rule(text).has_errors(), "{text}");
    }
    assert!(search("").matches_all());
    assert!(search("   ").matches_all());
    assert!(search("-").matches_all());
}

#[test]
fn quotes_make_phrases() {
    assert_eq!(search("\"Ach  Gott\"").root, word("ach gott"));
    assert_eq!(
        search("„Ach Gott“").root,
        Node::And(vec![word("\"ach"), word("gott\"")])
    );
    let query = search(r#"title:"ach \"gott\"""#);
    let Node::Term(term) = &query.root else {
        panic!()
    };
    assert_eq!(term.test, Test::Contains("ach \"gott\"".to_string()));
    // OR, AND and parentheses in quotes are text
    assert_eq!(search("\"a OR (b)\"").root, word("a or (b)"));
}

#[test]
fn unknown_names_before_a_colon_are_text() {
    assert_eq!(
        search("Re: x").root,
        Node::And(vec![word("re:"), word("x")])
    );
    assert!(search("Re: x").problems().is_empty());
    assert_eq!(search("12:30").root, word("12:30"));
    // A pasted "This Is: Bach" isn't the flag
    assert_eq!(
        search("This Is: Bach").root,
        Node::And(vec![word("this"), word("is:"), word("bach")])
    );
    // A name close to a field's gets a hint
    let query = search("composr:bach");
    assert_eq!(query.root, word("composr:bach"));
    assert_eq!(
        kinds(&query),
        [ProblemKind::DidYouMean {
            typed: "composr".to_string(),
            field: "composer"
        }]
    );
    assert!(!query.has_errors());
    assert!(rule("composr:bach").has_errors());
}

#[test]
fn hints_dont_stop_a_search() {
    let query = search("!");
    assert_eq!(kinds(&query), [ProblemKind::LoneBang]);
    assert!(!query.has_errors());
    let query = search(">2000");
    assert_eq!(query.root, word(">2000"));
    assert_eq!(kinds(&query), [ProblemKind::NoFieldToCompare]);
    let query = search("sort:-plays bach limit:10");
    assert_eq!(query.root, word("bach"));
    assert_eq!(kinds(&query), [ProblemKind::DirectiveInSearch]);
    assert!(!query.has_errors());
    assert_eq!(*query.directives(), Directives::default());
}

#[test]
fn spaces_after_a_fields_colon_are_skipped() {
    let candidate = track("", BACH);
    assert!(finds("Composer: Bach", &candidate));
    assert!(finds("composer:  bach", &candidate));
    // But not into another field's term
    assert_eq!(search("composer: year:1985").root, search("year:1985").root);
    // While it's typed, a field without a value matches everything
    assert!(search("bach composer:").problems().is_empty());
    assert_eq!(search("bach composer:").root, word("bach"));
    assert!(rule("bach composer:").has_errors());
}

#[test]
fn text_fields_contain_or_equal() {
    let candidate = track("", BACH);
    assert!(finds("composer:bach", &candidate));
    assert!(finds("composer:=\"johann sebastian bach\"", &candidate));
    assert!(finds("composer:==\"Bach, Johann Sebastian\"", &candidate));
    assert!(!finds("composer:=bach", &candidate));
    assert!(finds("people:goebel", &candidate));
    assert!(finds("performer:=\"reinhard goebel\"", &candidate));
    assert!(finds("performer:\"goebel (violin)\"", &candidate));
    assert!(!finds("performer:=reinhard", &candidate));
    assert!(finds("form:cantata genre:baroque", &candidate));
    let query = search("title:<b");
    assert_eq!(kinds(&query), [ProblemKind::CantCompare(Field::Title)]);
    assert!(query.matches_none());
}

#[test]
fn not_equal_is_minus_equal() {
    let without = track("", "yr\t1985\n");
    let candidate = track("", BACH);
    // On a track without the field, both are true
    assert!(finds("composer:!=bach", &without));
    assert!(finds("-composer:=bach", &without));
    // On a track with two values, both are true unless a value is equal
    assert!(finds("composer:!=telemann", &candidate));
    assert!(!finds("composer:<>\"johann sebastian bach\"", &candidate));
    assert_eq!(
        search("composer:!=bach").root,
        search("-composer:=bach").root
    );
}

#[test]
fn missing_values_are_false_and_true_under_minus() {
    let without = track("", "ti\tsong\n");
    assert!(!finds("year:1985", &without));
    assert!(finds("-year:1985", &without));
    assert!(!finds("year:<2000", &without));
    assert!(finds("-composer:bach", &without));
    assert!(finds("has:title", &without));
    assert!(!finds("has:composer", &without));
    assert!(finds("-has:composer", &without));
    assert!(finds("has:year", &track("", BACH)));
    assert!(search("has:nothing").has_errors());
    // To find tracks without a field, a rule writes -has:
    assert!(rule("composer:=\"\"").has_errors());
}

#[test]
fn numbers_compare_and_take_ranges() {
    let candidate = track("", BACH);
    for (query, found) in [
        ("year:1985", true),
        ("year:=1985", true),
        ("year:<1990", true),
        ("year:<1985", false),
        ("year:<=1985", true),
        ("year:>1984", true),
        ("year:>=1986", false),
        ("year:1980..1990", true),
        ("year:1986..", false),
        ("year:..1985", true),
    ] {
        assert_eq!(finds(query, &candidate), found, "{query}");
    }
    assert!(search("year:abc").has_errors());
    assert!(search("year:..").has_errors());
    assert!(search("year:>1980..1990").has_errors());
    // A reversed range reads as open while it's typed at the end
    assert!(finds("year:1980..1", &candidate));
    assert_eq!(
        kinds(&search("year:1990..1980 bach")),
        [ProblemKind::ReversedRange]
    );
}

#[test]
fn lengths_are_compared_in_seconds() {
    let candidate = track("", BACH);
    assert!(finds("length:>5min", &candidate));
    assert!(finds("length:4:00..5:01", &candidate));
    assert!(!finds("length:<5:01", &candidate));
    assert!(finds("length:301", &candidate));
    assert_eq!(
        kinds(&search("length:5m")),
        [ProblemKind::NotALength(Field::Length)]
    );
}

#[test]
fn dates_are_periods_and_relative_ones_moments() {
    let candidate = track("", BACH);
    for (query, found) in [
        ("released:1986", true),
        ("released:1986-11", true),
        ("released:1986-11-04", true),
        ("released:1986-12", false),
        ("released:<1987", true),
        ("released:<1986", false),
        ("released:>1985", true),
        ("released:>1986", false),
        ("released:1980..1986", true),
        ("released:1987..", false),
        // Since then; NOW is in 2026
        ("released:-50y", true),
        ("released:-30y", false),
        ("released:<-30y", true),
        ("released:..-30y", true),
    ] {
        assert_eq!(finds(query, &candidate), found, "{query}");
    }
    let span = track("", "rd\t1937-11-08/1938-03-08\n");
    assert!(finds("recorded:1938", &span));
    assert!(finds("recorded:1937-12", &span));
    assert!(!finds("recorded:1938-04", &span));
    assert!(search("released:yesterday").has_errors());
    assert!(search("released:-3x").has_errors());
}

#[test]
fn parses_of_relative_dates_are_equal() {
    // The time isn't worked out until the query is matched, so that the
    // album search can tell results for the search that's shown
    let first = search("released:-7d");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert_eq!(first, search("released:-7d"));
    let Node::Term(term) = &first.root else {
        panic!()
    };
    assert_eq!(
        term.test,
        Test::Dates(DateRange {
            from: Some(values::DatePoint::Ago {
                count: 7,
                unit: values::DateUnit::Day
            }),
            until: None,
        })
    );
    // An absolute one is worked out in local time
    let Node::Term(term) = &search("released:2026").root else {
        panic!()
    };
    let (start, end) = period("2026").unwrap();
    assert_eq!(
        term.test,
        Test::Dates(DateRange {
            from: Some(values::DatePoint::At(start)),
            until: Some(values::DatePoint::At(end)),
        })
    );
}

#[test]
fn catalogue_numbers_are_found_whatever_their_spacing() {
    let candidate = track("", BACH);
    assert!(finds("catalogue:bwv1047", &candidate));
    assert!(finds("catalogue:\"BWV 1047\"", &candidate));
    assert!(finds("catalogue:bwv", &candidate));
    assert!(finds("catalogue:bwv1046..1051", &candidate));
    assert!(!finds("catalogue:bwv1048..", &candidate));
    assert!(!finds("catalogue:k", &candidate));
    assert!(search("catalogue:nothing").has_errors());
}

#[test]
fn flags_and_the_tracks_own_state() {
    let candidate = track("", BACH);
    assert!(finds("is:live", &candidate));
    assert!(!finds("is:partial", &candidate));
    assert!(finds("-is:partial", &candidate));
    let favorite = Candidate {
        favorite: true,
        missing: true,
        ..candidate
    };
    assert!(finds("is:favorite is:missing", &favorite));
    assert!(!finds("is:favorite", &candidate));
    assert_eq!(
        kinds(&search("is:loved")),
        [ProblemKind::UnknownFlag("loved".to_string())]
    );
}

#[test]
fn paths_are_the_shown_ones() {
    let path = std::path::Path::new("/music/Bach/Cantatas/BWV 1.flac");
    let candidate = Candidate {
        path: Some(path),
        ..Default::default()
    };
    assert!(finds("path:bach/cantatas", &candidate));
    assert!(finds("path:/music/bach", &candidate));
    assert!(!finds("path:ach/cantatas", &candidate));
    assert!(!finds("path:/bach", &candidate));
    // A word with a "/" is a plain word
    assert_eq!(search("bach/cantatas").root, word("bach/cantatas"));
}

#[test]
fn field_groups_apply_the_field_to_each_value() {
    let candidate = track("", BACH);
    assert_eq!(
        search("composer:(bach OR handel)").root,
        search("composer:bach OR composer:handel").root
    );
    assert_eq!(
        search("composer:(bach -handel)").root,
        search("composer:bach -composer:handel").root
    );
    assert!(finds(
        "composer:(handel OR bach) year:(<1960 OR >1980)",
        &candidate
    ));
    assert!(!finds("year:(<1960 OR >1990)", &candidate));
    assert_eq!(
        kinds(&search("composer:(title:x)")),
        [ProblemKind::FieldInGroup]
    );
    // A space before the parenthesis is skipped as before a value
    assert_eq!(
        search("composer: (bach OR handel)").root,
        search("composer:(bach OR handel)").root
    );
}

#[test]
fn errors_make_the_query_match_nothing() {
    let candidate = track("johann sebastian bach live", BACH);
    for text in [
        "year:abc",
        "bach year:abc",
        "bach OR year:abc",
        "-(year:abc OR live)",
        "-year:abc -live",
        "composer::bach",
    ] {
        let query = search(text);
        assert!(query.has_errors(), "{text}");
        assert!(query.matches_none(), "{text}");
        assert!(!query.matches_all(), "{text}");
        assert!(!query.matches(&candidate, &NOW), "{text}");
    }
    assert_eq!(kinds(&search("composer::x")), [ProblemKind::Reserved]);
}

#[test]
fn plain_words_search_the_text_of_the_tags() {
    let candidate = track("johann sebastian bach brandenburg concerto bwv1047", "");
    assert!(finds("bach", &candidate));
    assert!(finds("Bach BRANDENBURG", &candidate));
    assert!(finds("bwv1047", &candidate));
    assert!(!finds("handel", &candidate));
    assert!(finds("handel OR bach", &candidate));
    assert!(!finds("-bach", &candidate));
    assert!(finds("\"sebastian bach\"", &candidate));
}

#[test]
fn deep_parentheses_are_capped() {
    let deep = format!("{}bach{}", "(".repeat(100), ")".repeat(100));
    let query = search(&deep);
    assert_eq!(kinds(&query), [ProblemKind::TooDeep]);
    assert!(!query.has_errors());
    assert!(rule(&deep).has_errors());
    // A long run of "-" doesn't go deep either
    let negated = format!("{}bach", "-".repeat(10_001));
    assert_eq!(search(&negated).root, Node::Not(Box::new(word("bach"))));
}

#[test]
fn rules_refuse_what_a_search_lets_through() {
    for text in [
        "composr:bach",
        "bach*",
        "title:a?",
        "@x",
        "[x]",
        "composer:",
        "\"unclosed",
        "(bach",
        "!",
        ">2000",
        "sort:nothing",
        "limit:0",
        "limit:many",
        "by:composer",
    ] {
        assert!(rule(text).has_errors(), "{text}");
    }
    // Quoted, the characters are fine
    assert!(!rule("\"bach*\"").has_errors());
    assert!(!rule("title:\"a?\"").has_errors());
}

#[test]
fn rules_take_directives() {
    let query = rule("composer:bach sort:-released,album pick:-year limit:1h30min by:album");
    assert!(query.problems().is_empty(), "{:?}", query.problems());
    assert_eq!(query.root, search("composer:bach").root);
    let directives = query.directives();
    assert_eq!(
        directives.sort,
        [
            SortKey {
                by: SortBy::Field(Field::Released),
                descending: true
            },
            SortKey {
                by: SortBy::Album,
                descending: false
            }
        ]
    );
    assert_eq!(
        directives.pick,
        [SortKey {
            by: SortBy::Year,
            descending: true
        }]
    );
    assert_eq!(directives.limit, Some(Limit::Duration(5400)));
    assert_eq!(directives.by, Some(Unit::Album));
    assert_eq!(
        rule("limit:100").directives().limit,
        Some(Limit::Count(100))
    );
    // Text fields other than the queue's sorts don't sort
    assert!(rule("sort:genre").has_errors());
}

// Every kind of node, as a rule could be written
const CORPUS: [&str; 18] = [
    "bach",
    "\"ach gott\" OR handel",
    "a OR b c",
    "(a OR b) c",
    "-(a b) -c",
    "composer:Bach year:<1990",
    "composer:!=bach",
    "title:\"Re: x\" \"re:x\"",
    "performer:==\"reinhard goebel\"",
    "year:1980..1990 length:>=5min",
    "released:-30d recorded:..1950",
    "catalogue:bwv1046..1051 catalogue:op2",
    "path:bach/cantatas path:/music",
    "has:composer -has:work is:live is:favorite",
    "composer:(bach OR handel)",
    "\"-live\" \">2000\"",
    "title:\"=x\" title:\"(x)\"",
    "bach sort:-released,album pick:random limit:1h30min by:work",
];

#[test]
fn print_then_parse_is_identity() {
    for text in CORPUS {
        let query = rule(text);
        assert!(
            query.problems().is_empty(),
            "{text}: {:?}",
            query.problems()
        );
        let printed = query.to_text();
        let again = rule(&printed);
        assert!(
            again.problems().is_empty(),
            "{printed}: {:?}",
            again.problems()
        );
        assert_eq!(again.root, query.root, "{text} -> {printed}");
        assert_eq!(again.directives, query.directives, "{text} -> {printed}");
        // And printing is stable
        assert_eq!(again.to_text(), printed, "{text}");
    }
    assert_eq!(rule("Composer:==Bach").to_text(), "composer:=Bach");
    assert_eq!(rule("year:<>1980").to_text(), "-year:=1980");
}

#[test]
fn rule_and_search_parse_agree_on_valid_text() {
    for text in CORPUS {
        let rule = rule(text);
        let search = search(text);
        assert_eq!(search.root, rule.root, "{text}");
    }
}

#[test]
fn queries_say_what_they_need() {
    assert_eq!(
        search("bach").needs(),
        Needs {
            search_key: true,
            ..Needs::default()
        }
    );
    let needs = search("composer:bach path:x is:favorite").needs();
    assert!(needs.fields && needs.path && needs.favorites && !needs.search_key);
    assert_eq!(search("is:missing").needs(), Needs::default());
}

#[test]
fn every_problem_has_a_message() {
    for text in [
        "year:abc",
        "released:x",
        "length:x",
        "catalogue:x",
        "title:<x",
        "year:2..1 x",
        "is:x",
        "has:x",
        "composer:(title:x)",
        "composer::x",
        "!",
        ">2",
        "composr:x",
        "sort:x",
    ] {
        for problem in search(text).problems().iter().chain(rule(text).problems()) {
            assert!(!problem.message().is_empty(), "{text}");
            assert!(
                !problem.message().contains('{'),
                "{text}: {}",
                problem.message()
            );
        }
    }
}

#[test]
fn names_are_told_apart_and_described() {
    let names: Vec<&str> = Field::ALL
        .iter()
        .map(|field| field.name())
        .chain(["is", "has"])
        .chain(Directive::ALL.iter().map(|directive| directive.name()))
        .collect();
    let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len());
    for field in Field::ALL {
        assert_eq!(Field::from_name(&field.name().to_uppercase()), Some(field));
        assert!(!field.description().is_empty());
    }
    for flag in IsFlag::ALL {
        assert_eq!(IsFlag::from_name(flag.name()), Some(flag));
        assert!(!flag.description().is_empty());
    }
}

#[test]
fn fields_from_the_library_and_the_play_stats() {
    const DAY: i64 = 24 * 60 * 60;
    let candidate = Candidate {
        added: Some(NOW.now - 10 * DAY),
        played: Some(NOW.now - 70 * DAY),
        favorited: Some(NOW.now - DAY),
        plays: Some(5),
        favorite_album: true,
        ..Default::default()
    };
    assert!(finds("added:-30d", &candidate));
    assert!(!finds("added:-7d", &candidate));
    assert!(finds("added:<-7d", &candidate));
    assert!(finds("played:<-61d plays:3..", &candidate));
    assert!(finds("favorited:-2d", &candidate));
    assert!(finds("is:favorite-album -is:favorite-work", &candidate));
    // A track never heard has no plays and wasn't played
    let never = Candidate {
        plays: Some(0),
        ..Default::default()
    };
    assert!(finds("plays:0 -has:played -has:plays", &never));
    assert!(!finds("played:..2030", &never));
    assert!(finds("-played:2026", &never));
    let needs = search("plays:3 added:-1y is:favorite-work favorited:2026").needs();
    assert!(needs.stats && needs.added && needs.favorite_works && needs.favorites);
    assert!(!needs.fields && !needs.search_key);
}
