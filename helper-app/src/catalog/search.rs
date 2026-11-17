//! How a query matches and ranks catalog entries.
//!
//! Every entry is searched by two fields: its *name*, and a *context* line
//! (bin path, effect folder, kind). Each whitespace-separated query term must
//! match the entry: fuzzily in the name, or as a literal substring of the
//! context, so a term can't scatter its letters across a long bin path. A
//! single-letter term must start a word, and a number must appear as written. Entries whose name matches every term
//! get bonuses for exact, prefix and whole-word matches. When a term matches
//! nowhere, a typo pass still accepts the entry if each query word is close to
//! the start of a word of its name (see [`max_typos`]).

use nucleo_matcher::{
    chars,
    pattern::{Atom, AtomKind, CaseMatching, Normalization},
    Matcher, Utf32String,
};

/// The name, or the name without its file extension, equals the query.
const EXACT_BONUS: u32 = 500;
/// The name starts with the query.
const PREFIX_BONUS: u32 = 200;
/// Each query word starts a word of the name, in the query's order.
const WORDS_IN_ORDER_BONUS: u32 = 150;
/// Each query word starts a word of the name, in any order.
const WORDS_BONUS: u32 = 110;
/// The name contains the query (ignoring spaces and punctuation).
const CONTAINS_BONUS: u32 = 80;
/// Score of a single-letter term that starts a word of the name.
const INITIAL_SCORE: u32 = 24;
/// Per-character score of a word matched by the typo pass.
const TYPO_CHAR_SCORE: u32 = 10;
/// What each edit costs a word matched by the typo pass.
const TYPO_EDIT_PENALTY: u32 = 25;
/// Words longer than this are only matched by prefix in the typo pass.
const MAX_TYPO_WORD: usize = 32;

/// Lowercases `c` and strips its accent, the way the fuzzy matcher compares characters.
fn fold(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        chars::to_lower_case(chars::normalize(c))
    }
}

/// Text split into lowercase, accent-folded words. Words break at spaces and
/// punctuation, at lower-to-upper case changes and between letters and digits,
/// so `A-Cam_Take02.mov` has the words `a cam take 02 mov`.
#[derive(Default)]
struct Words {
    /// Every word, concatenated: the text without spaces or punctuation.
    chars: Box<[char]>,
    /// Where each word ends in `chars`.
    ends: Box<[u32]>,
}

impl Words {
    fn new(text: &str) -> Self {
        let mut chars = Vec::with_capacity(text.len());
        let mut ends = Vec::new();
        let mut prev: Option<char> = None;
        let close = |chars: &Vec<char>, ends: &mut Vec<u32>| {
            if chars.len() > ends.last().map_or(0, |&end| end as usize) {
                ends.push(chars.len() as u32);
            }
        };
        for c in text.chars() {
            if !c.is_alphanumeric() {
                close(&chars, &mut ends);
                prev = None;
                continue;
            }
            if let Some(p) = prev {
                if (p.is_lowercase() && c.is_uppercase()) || p.is_numeric() != c.is_numeric() {
                    close(&chars, &mut ends);
                }
            }
            chars.push(fold(c));
            prev = Some(c);
        }
        close(&chars, &mut ends);
        Self {
            chars: chars.into(),
            ends: ends.into(),
        }
    }

    fn compact(&self) -> &[char] {
        &self.chars
    }

    fn iter(&self) -> impl Iterator<Item = &[char]> + '_ {
        let starts = std::iter::once(0).chain(self.ends.iter().copied());
        starts
            .zip(self.ends.iter().copied())
            .map(|(start, end)| &self.chars[start as usize..end as usize])
    }

    fn has_initial(&self, initial: char) -> bool {
        self.iter().any(|word| word.first() == Some(&initial))
    }
}

/// The precomputed text one catalog entry is searched by.
pub(super) struct SearchKey {
    name: Utf32String,
    context: Utf32String,
    name_words: Words,
    context_words: Words,
    /// Length of the name's compact form without its file extension.
    stem_len: usize,
}

impl SearchKey {
    pub(super) fn new(name: &str, context: &str) -> Self {
        let name_words = Words::new(name);
        let extension_len = name
            .rsplit_once('.')
            .map(|(stem, ext)| (stem, ext.len()))
            .filter(|&(stem, len)| {
                let ext = &name[name.len() - len..];
                !stem.is_empty()
                    && (1..=5).contains(&len)
                    && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                    && ext.bytes().any(|b| b.is_ascii_alphabetic())
            })
            .map_or(0, |(_, len)| len);
        let stem_len = name_words.compact().len().saturating_sub(extension_len);
        Self {
            name: name.into(),
            context: context.into(),
            name_words,
            context_words: Words::new(context),
            stem_len,
        }
    }
}

enum Term {
    /// A term of two or more characters.
    Text { name: Atom, context: Atom },
    /// A single letter or digit, which must start a word.
    Initial(char),
}

/// A parsed, non-blank search query.
pub(super) struct Query {
    terms: Vec<Term>,
    words: Words,
    /// Whether any query word is long enough for the typo pass.
    typo: bool,
}

impl Query {
    /// Parses `text`, or returns `None` when it is blank.
    pub(super) fn new(text: &str) -> Option<Self> {
        let tokens: Vec<&str> = text.split_whitespace().collect();
        if tokens.is_empty() {
            return None;
        }
        // Lone punctuation like the `&` in `brightness & contrast` is dropped,
        // unless the query has nothing else.
        let has_word = |token: &&str| token.chars().any(char::is_alphanumeric);
        let keep_punctuation = !tokens.iter().any(has_word);
        let terms = tokens
            .iter()
            .filter(|token| keep_punctuation || has_word(token))
            .map(|token| {
                let mut chars = token.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if c.is_alphanumeric() => {
                        Term::Initial(fold(c))
                    }
                    _ => {
                        let atom = |kind| {
                            Atom::new(token, CaseMatching::Ignore, Normalization::Smart, kind, false)
                        };
                        // A number is a take, shot or version: `0042` must not
                        // match the scattered digits of `0044_take2`.
                        let numeric = token.chars().all(|c| c.is_numeric());
                        Term::Text {
                            name: atom(if numeric { AtomKind::Substring } else { AtomKind::Fuzzy }),
                            context: atom(AtomKind::Substring),
                        }
                    }
                }
            })
            .collect();
        let words = Words::new(text);
        let typo = words.iter().any(|word| max_typos(word) > 0);
        Some(Self { terms, words, typo })
    }

    /// The entry's relevance to this query, or `None` when it doesn't match.
    pub(super) fn score(&self, key: &SearchKey, matcher: &mut Matcher) -> Option<u32> {
        let mut total = 0;
        let mut name_only = true;
        for term in &self.terms {
            // (score, whether the term matched the name rather than the context)
            let matched = match term {
                Term::Initial(c) => {
                    if key.name_words.has_initial(*c) {
                        Some((INITIAL_SCORE, true))
                    } else {
                        key.context_words.has_initial(*c).then_some((INITIAL_SCORE, false))
                    }
                }
                Term::Text { name, context } => name
                    .score(key.name.slice(..), matcher)
                    .map(|score| (u32::from(score), true))
                    .or_else(|| {
                        context
                            .score(key.context.slice(..), matcher)
                            .map(|score| (u32::from(score), false))
                    }),
            };
            match matched {
                Some((score, true)) => total += score,
                Some((score, false)) => {
                    total += score / 2;
                    name_only = false;
                }
                None => return self.typo_score(key),
            }
        }
        if name_only {
            total += self.name_bonus(key);
        }
        Some(total)
    }

    fn name_bonus(&self, key: &SearchKey) -> u32 {
        let query = self.words.compact();
        let name = key.name_words.compact();
        if query.is_empty() {
            0
        } else if name == query || name[..key.stem_len] == *query {
            EXACT_BONUS
        } else if name.starts_with(query) {
            PREFIX_BONUS
        } else if self.words_start_name_words_in_order(key) {
            WORDS_IN_ORDER_BONUS
        } else if self
            .words
            .iter()
            .all(|word| key.name_words.iter().any(|name_word| name_word.starts_with(word)))
        {
            WORDS_BONUS
        } else if query.len() >= 2 && name.windows(query.len()).any(|window| window == query) {
            CONTAINS_BONUS
        } else {
            0
        }
    }

    fn words_start_name_words_in_order(&self, key: &SearchKey) -> bool {
        let mut name_words = key.name_words.iter();
        self.words
            .iter()
            .all(|word| name_words.any(|name_word| name_word.starts_with(word)))
    }

    fn typo_score(&self, key: &SearchKey) -> Option<u32> {
        if !self.typo {
            return None;
        }
        self.words.iter().try_fold(0, |total, word| {
            let max_edits = max_typos(word);
            let edits = key
                .name_words
                .iter()
                .filter_map(|name_word| prefix_edit_distance(word, name_word, max_edits))
                .min()?;
            let score = (word.len() as u32 * TYPO_CHAR_SCORE).saturating_sub(edits * TYPO_EDIT_PENALTY);
            Some(total + score.max(1))
        })
    }
}

/// How many typos a query word may have: none for short words, where one edit
/// reaches too many other words (`crop` → `drop`), and none for numbers, where
/// a near miss is a different take or shot.
fn max_typos(word: &[char]) -> u32 {
    if word.iter().all(|c| c.is_numeric()) {
        return 0;
    }
    match word.len() {
        0..=4 => 0,
        5..=8 => 1,
        _ => 2,
    }
}

/// The fewest edits (insertions, deletions, substitutions or swaps of adjacent
/// characters) that turn `query` into a prefix of `word`, if at most `max`.
fn prefix_edit_distance(query: &[char], word: &[char], max: u32) -> Option<u32> {
    if word.starts_with(query) {
        return Some(0);
    }
    let m = query.len();
    if max == 0 || m > MAX_TYPO_WORD || word.len() + (max as usize) < m {
        return None;
    }
    let n = word.len().min(m + max as usize);
    const COLS: usize = MAX_TYPO_WORD + 3;
    let mut before = [0u32; COLS];
    let mut prev: [u32; COLS] = std::array::from_fn(|j| j as u32);
    let mut cur = [0u32; COLS];
    for i in 1..=m {
        cur[0] = i as u32;
        let mut row_min = cur[0];
        for j in 1..=n {
            let cost = u32::from(query[i - 1] != word[j - 1]);
            let mut best = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && query[i - 1] == word[j - 2] && query[i - 2] == word[j - 1] {
                best = best.min(before[j - 2] + 1);
            }
            cur[j] = best;
            row_min = row_min.min(best);
        }
        if row_min > max {
            return None;
        }
        before = prev;
        prev = cur;
    }
    prev[..=n].iter().copied().min().filter(|&edits| edits <= max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        Words::new(text).iter().map(|word| word.iter().collect()).collect()
    }

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn words_split_at_punctuation_case_and_digits_and_fold_accents() {
        assert_eq!(words("A-Cam_Interview02.mov"), ["a", "cam", "interview", "02", "mov"]);
        assert_eq!(words("DeNoise"), ["de", "noise"]);
        assert_eq!(words("Color Balance (HLS)"), ["color", "balance", "hls"]);
        assert_eq!(words("Brightness & Contrast"), ["brightness", "contrast"]);
        assert_eq!(words("Café Scène"), ["cafe", "scene"]);
        assert_eq!(words("Basic 3D"), ["basic", "3", "d"]);
        assert!(words("  & / ").is_empty());
        assert_eq!(Words::new("Single-band").compact(), chars("singleband"));
    }

    #[test]
    fn file_extensions_are_left_out_of_exact_matches() {
        for (name, stem_len) in [
            ("A-Cam.mov", 4),
            ("clip.mp4", 4),
            ("Interview", 9),
            ("Take 1.5", 6),
            (".mov", 3),
            ("Archive.backup", 13),
        ] {
            assert_eq!(SearchKey::new(name, "").stem_len, stem_len, "{name}");
        }
    }

    #[test]
    fn prefix_edit_distance_counts_typos_against_a_word_start() {
        let distance = |query: &str, word: &str, max| prefix_edit_distance(&chars(query), &chars(word), max);
        assert_eq!(distance("gauss", "gaussian", 1), Some(0));
        assert_eq!(distance("guassian", "gaussian", 1), Some(1));
        assert_eq!(distance("interveiw", "interview", 2), Some(1));
        assert_eq!(distance("lumetir", "lumetri", 1), Some(1));
        assert_eq!(distance("blyr", "blur", 1), Some(1));
        assert_eq!(distance("crss", "cross", 1), Some(1));
        assert_eq!(distance("gausain", "gaussian", 1), None);
        assert_eq!(distance("gausain", "gaussian", 2), Some(2));
        assert_eq!(distance("mosiac", "mosaic", 1), Some(1));
        assert_eq!(distance("tint", "track", 1), None);
        assert_eq!(distance("blur", "blxx", 1), None);
        assert_eq!(distance("blur", "bl", 1), None);
        assert_eq!(distance("blyr", "blur", 0), None);
        let long = "a".repeat(MAX_TYPO_WORD + 1);
        assert_eq!(distance(&long, &format!("b{long}"), 2), None);
    }

    #[test]
    fn query_terms_ignore_lone_punctuation_and_need_a_word_start_for_one_letter() {
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let score = |query: &str, name: &str, context: &str, matcher: &mut Matcher| {
            Query::new(query)?.score(&SearchKey::new(name, context), matcher)
        };
        assert!(Query::new("   ").is_none());
        assert!(score("brightness & contrast", "Brightness & Contrast", "", &mut matcher).is_some());
        assert!(score("&", "Brightness & Contrast", "", &mut matcher).is_some());
        assert!(score("a cam", "A-Cam.mov", "", &mut matcher).is_some());
        assert!(score("a cam", "Camera Blur", "", &mut matcher).is_none());
        assert!(score("c", "Lumetri Color", "", &mut matcher).is_some());
        assert!(score("c", "Tint", "Color Correction", &mut matcher).is_some());
        assert!(score("c", "Mosaic", "Stylize", &mut matcher).is_none());
        // Context terms match literally, not as scattered letters.
        assert!(score("cafe", "Clip_0001.mov", "Footage / Cards / Card A", &mut matcher).is_none());
        assert!(score("cards", "Clip_0001.mov", "Footage / Cards / Card A", &mut matcher).is_some());
    }

    #[test]
    fn name_bonuses_rank_exact_then_prefix_then_word_then_substring_matches() {
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let query = Query::new("blur").unwrap();
        let score = |name: &str, context: &str, matcher: &mut Matcher| {
            query.score(&SearchKey::new(name, context), matcher).unwrap()
        };
        let exact = score("Blur", "", &mut matcher);
        let stem = score("blur.mov", "", &mut matcher);
        let prefix = score("Blurred Lines", "", &mut matcher);
        let word = score("Gaussian Blur", "", &mut matcher);
        let inside = score("Deblurrer", "", &mut matcher);
        let context = score("Sharpen", "Blur & Sharpen", &mut matcher);
        assert!(exact > prefix && stem > prefix, "{exact} {stem} {prefix}");
        assert!(prefix > word, "{prefix} {word}");
        assert!(word > inside, "{word} {inside}");
        assert!(inside > context, "{inside} {context}");
    }

    #[test]
    fn typos_match_below_clean_matches_and_only_for_long_enough_words() {
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let score = |query: &str, name: &str, matcher: &mut Matcher| {
            Query::new(query)?.score(&SearchKey::new(name, ""), matcher)
        };
        let typo = score("guassian", "Gaussian Blur", &mut matcher).unwrap();
        let clean = score("gaussian", "Gaussian Blur", &mut matcher).unwrap();
        assert!(clean > typo, "{clean} {typo}");
        assert!(score("guassian blur", "Gaussian Blur", &mut matcher).is_some());
        assert!(score("interveiw", "A-Cam_Interview_01.mov", &mut matcher).is_some());
        assert!(score("mosiac", "Mosaic", &mut matcher).is_some());
        assert!(score("blru", "Blur", &mut matcher).is_none(), "four-letter words need an exact prefix");
        assert!(score("crop", "Drop Shadow", &mut matcher).is_none());
        assert!(score("shot 00124", "Shot_00123.mov", &mut matcher).is_none(), "numbers never fuzz");
        assert!(score("0042", "Clip_0044_take2.mov", &mut matcher).is_none());
        assert!(score("0042", "Clip_0042_take2.mov", &mut matcher).is_some());
        assert!(score("guassian", "Camera Blur", &mut matcher).is_none());
        assert!(score("guassian zzz", "Gaussian Blur", &mut matcher).is_none());
    }
}
