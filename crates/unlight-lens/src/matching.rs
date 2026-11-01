//! Decides whether a lens name from a file and a database entry mean the same lens. Names
//! vary in spelling (`F4` or `f/4`, with or without the maker) but differ meaningfully in
//! numbers and generation (`II`). Being lenient about the first and strict about the second
//! is what keeps a wrong profile off a photo.

use std::collections::BTreeSet;

const VERSIONS: [&str; 7] = ["ii", "iii", "iv", "v", "vi", "vii", "viii"];

#[derive(Debug, Default, PartialEq)]
struct Tokens {
    numbers: Vec<String>,
    versions: BTreeSet<String>,
    words: BTreeSet<String>,
}

fn push_token(tokens: &mut Tokens, token: &str) {
    if token.is_empty() || token == "f" || token == "mm" {
        return;
    }
    if token.starts_with(|c: char| c.is_ascii_digit()) {
        let trimmed = if token.contains('.') { token.trim_end_matches('0').trim_end_matches('.') } else { token };
        tokens.numbers.push(trimmed.to_owned());
    } else if VERSIONS.contains(&token) {
        tokens.versions.insert(token.to_owned());
    } else {
        tokens.words.insert(token.to_owned());
    }
}

/// Splits at punctuation and wherever letters meet digits, so `XF16-55mmF2.8` becomes `xf`,
/// `16`, `55` and `2.8`.
fn tokenize(name: &str) -> Tokens {
    let mut tokens = Tokens::default();
    let mut current = String::new();
    let lower = name.to_lowercase();
    let mut characters = lower.chars().peekable();
    while let Some(character) = characters.next() {
        let is_decimal = character == '.'
            && current.starts_with(|c: char| c.is_ascii_digit())
            && characters.peek().is_some_and(char::is_ascii_digit);
        let joins = is_decimal
            || current.is_empty() && character.is_alphanumeric()
            || current.chars().next_back().is_some_and(|last| {
                last.is_ascii_digit() && character.is_ascii_digit()
                    || last == '.' && character.is_ascii_digit()
                    || last.is_alphabetic() && character.is_alphabetic()
            });
        if joins {
            current.push(character);
            continue;
        }
        push_token(&mut tokens, &current);
        current.clear();
        if character.is_alphanumeric() {
            current.push(character);
        }
    }
    push_token(&mut tokens, &current);
    tokens
}

fn without_maker(mut tokens: Tokens, maker: &Tokens) -> Tokens {
    tokens.words.retain(|word| !maker.words.contains(word));
    tokens
}

fn is_subset(smaller: &[String], larger: &[String]) -> bool {
    smaller.iter().all(|value| larger.contains(value))
}

fn numbers_agree(a: &[String], b: &[String]) -> bool {
    is_subset(a, b) || is_subset(b, a)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    None,
    Compatible,
    Exact,
}

fn fit(query: &str, name: &str, maker: &str) -> Fit {
    let maker = tokenize(maker);
    let query = without_maker(tokenize(query), &maker);
    let name = without_maker(tokenize(name), &maker);
    if query == name {
        return Fit::Exact;
    }
    let same_generation = query.versions == name.versions;
    let words_agree = query.words.is_subset(&name.words) || name.words.is_subset(&query.words);
    if same_generation && words_agree && numbers_agree(&query.numbers, &name.numbers) {
        Fit::Compatible
    } else {
        Fit::None
    }
}

/// One database entry. A lens can be listed under several translated names.
pub(crate) struct Candidate<'a, T> {
    pub names: Vec<String>,
    pub maker: String,
    pub value: &'a T,
}

#[derive(Debug, PartialEq)]
pub(crate) enum Choice<'a, T> {
    Found(&'a T),
    Ambiguous(Vec<String>),
    Missing,
}

/// Prefers an exact match. Short of that, a compatible name is only trusted if it's the only
/// one, since a bare `24-70mm` fits a dozen different lenses.
pub(crate) fn choose<'a, T>(query: &str, candidates: &[Candidate<'a, T>]) -> Choice<'a, T> {
    let fits: Vec<(Fit, &Candidate<T>)> = candidates
        .iter()
        .map(|candidate| {
            let best = candidate.names.iter().map(|name| fit(query, name, &candidate.maker)).max();
            (best.unwrap_or(Fit::None), candidate)
        })
        .collect();
    if let Some((_, exact)) = fits.iter().find(|(fit, _)| *fit == Fit::Exact) {
        return Choice::Found(exact.value);
    }
    let compatible: Vec<&Candidate<T>> =
        fits.iter().filter(|(fit, _)| *fit == Fit::Compatible).map(|&(_, candidate)| candidate).collect();
    let distinct: BTreeSet<&str> =
        compatible.iter().filter_map(|candidate| candidate.names.first()).map(String::as_str).collect();
    match (compatible.first(), distinct.len()) {
        (Some(only), 1) => Choice::Found(only.value),
        (_, 0) => Choice::Missing,
        _ => Choice::Ambiguous(distinct.into_iter().map(str::to_owned).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lens(name: &str) -> (Vec<String>, &'static str) {
        (vec![name.to_owned()], "")
    }

    fn pick(query: &str, entries: &[(&str, &str)]) -> Option<usize> {
        let indices: Vec<usize> = (0..entries.len()).collect();
        let candidates: Vec<Candidate<usize>> = entries
            .iter()
            .zip(&indices)
            .map(|(&(name, maker), value)| Candidate {
                names: vec![name.to_owned()],
                maker: maker.to_owned(),
                value,
            })
            .collect();
        match choose(query, &candidates) {
            Choice::Found(&index) => Some(index),
            _ => None,
        }
    }

    #[test]
    fn spelling_differences_do_not_matter() {
        for (query, entry, maker) in [
            ("FE 24-70mm F4 ZA OSS", "FE 24-70mm f/4 ZA OSS", "Sony"),
            ("NIKKOR Z 24-70mm f/4 S", "Nikkor Z 24-70mm f/4 S", "Nikon"),
            ("XF16-55mmF2.8 R LM WR", "XF 16-55mm f/2.8 R LM WR", "Fujifilm"),
            ("RF85mm F2 MACRO IS STM", "Canon RF 85mm F2 MACRO IS STM", "Canon"),
            ("EF-M15-45mm f/3.5-6.3 IS STM", "Canon EF-M 15-45mm f/3.5-6.3 IS STM", "Canon"),
            ("LEICA DG 12-60/F2.8-4.0", "LEICA DG 12-60/F2.8-4.0", "Panasonic"),
        ] {
            assert_eq!(pick(query, &[(entry, maker)]), Some(0), "{query}");
        }
    }

    #[test]
    fn an_exact_name_beats_its_successor() {
        let entries = [("XF 16-55mm f/2.8 R LM WR II", "Fujifilm"), ("XF 16-55mm f/2.8 R LM WR", "Fujifilm")];
        assert_eq!(pick("XF16-55mmF2.8 R LM WR", &entries), Some(1));
    }

    #[test]
    fn a_later_generation_is_a_different_lens() {
        let entries = [("Canon EF 24-70mm f/2.8L II USM", "Canon")];
        assert_eq!(pick("EF24-70mm f/2.8L USM", &entries), None);
    }

    #[test]
    fn different_numbers_are_different_lenses() {
        assert_eq!(pick("XF16-50mmF2.8-4.8 R LM WR", &[("XF 16-55mm f/2.8 R LM WR", "Fujifilm")]), None);
    }

    #[test]
    fn a_bare_focal_range_stays_ambiguous() {
        let entries = [
            ("Canon EF 24-70mm f/2.8L USM", "Canon"),
            ("Tamron SP 24-70mm f/2.8 Di VC USD", "Tamron"),
            ("Canon EF 24-70mm f/4L IS USM", "Canon"),
        ];
        let indices = [0, 1, 2];
        let candidates: Vec<Candidate<i32>> = entries
            .iter()
            .zip(&indices)
            .map(|(&(name, maker), value)| Candidate { names: vec![name.to_owned()], maker: maker.to_owned(), value })
            .collect();
        assert!(matches!(choose("24-70mm", &candidates), Choice::Ambiguous(names) if names.len() == 3));
    }

    #[test]
    fn a_detailed_name_can_match_a_shorter_entry_when_it_is_the_only_one() {
        let entries = [("Sigma 24-70mm f/2.8 DG DN Art", "Sigma")];
        assert_eq!(pick("24-70mm F2.8 DG DN | Art 019", &entries), Some(0));
    }

    #[test]
    fn nothing_alike_is_missing() {
        let (names, maker) = lens("Canon EF 50mm f/1.8");
        let value = 0;
        let candidates = [Candidate { names, maker: maker.to_owned(), value: &value }];
        assert!(matches!(choose("Mystery 85mm", &candidates), Choice::Missing));
    }

    #[test]
    fn repeated_entries_of_one_lens_are_not_ambiguous() {
        let entries = [("Canon EF 50mm f/1.8 STM", "Canon"), ("Canon EF 50mm f/1.8 STM", "Canon")];
        assert_eq!(pick("EF50mm f/1.8", &entries), Some(0));
    }
}
