//! Matching a query against lines of text.

// Imports
use crate::{Bounds, CharBox, Line};

/// Text to search for.
#[derive(Debug, Clone)]
pub struct Query {
    chars: Vec<char>,
    has_zhuyin: bool,
}

/// A place in a line where a query matches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    /// The bounds of the matched characters.
    pub bounds: Bounds,
    /// Higher is better. Matches on the most likely readings score above matches on other candidates.
    pub score: f32,
}

impl Query {
    /// Returns `None` when the text holds nothing to search for.
    pub fn new(text: &str) -> Option<Self> {
        let chars = text
            .chars()
            .filter(|&c| !is_ignored(c))
            .map(fold_case)
            .collect::<Vec<char>>();
        (!chars.is_empty()).then(|| Self {
            has_zhuyin: chars.iter().any(|&c| is_zhuyin(c)),
            chars,
        })
    }

    /// The places where the query matches in the line.
    ///
    /// It matches where its characters appear in a row, each among the candidates of its position. Whitespace and
    /// zhuyin tone marks are ignored and letter case does not matter. A query without zhuyin also matches across
    /// zhuyin symbols, which is what ruby zhuyin beside characters is read as.
    pub fn find(&self, line: &Line) -> Vec<Match> {
        let chars = line
            .chars
            .iter()
            .filter(|c| c.top().is_some_and(|ch| !is_ignored(ch)))
            .collect::<Vec<&CharBox>>();
        let without_zhuyin = chars
            .iter()
            .copied()
            .filter(|c| !c.top().is_some_and(is_zhuyin))
            .collect::<Vec<&CharBox>>();
        let mut sequences = vec![&chars];
        if !self.has_zhuyin && without_zhuyin.len() < chars.len() {
            sequences.push(&without_zhuyin);
        }

        let mut matches: Vec<Match> = Vec::new();
        for window in sequences.iter().flat_map(|s| s.windows(self.chars.len())) {
            let Some(score) = self.score(window) else {
                continue;
            };
            let bounds = Bounds {
                x: window[0].x0,
                w: window[window.len() - 1].x1 - window[0].x0,
                ..line.bounds
            };
            // The same place can match with and without the zhuyin left out
            if !matches.iter().any(|m| m.bounds == bounds) {
                matches.push(Match { bounds, score });
            }
        }
        matches
    }

    /// Scores the characters against the query, or returns `None` when they do not match.
    ///
    /// The score is the mean confidence of the matched readings, plus one when all are the most likely reading.
    fn score(&self, chars: &[&CharBox]) -> Option<f32> {
        let mut exact = true;
        let mut confidence = 0.0;
        for (c, &wanted) in chars.iter().zip(self.chars.iter()) {
            let position = c
                .candidates
                .iter()
                .position(|candidate| fold_case(candidate.ch) == wanted)?;
            exact &= position == 0;
            confidence += c.candidates[position].confidence;
        }
        Some(confidence / self.chars.len() as f32 + if exact { 1.0 } else { 0.0 })
    }
}

/// Whether the character plays no part in matching. Tone marks are small and often not read, or not typed.
fn is_ignored(c: char) -> bool {
    c.is_whitespace() || matches!(c, 'ˊ' | 'ˇ' | 'ˋ' | '˙')
}

fn is_zhuyin(c: char) -> bool {
    ('\u{3105}'..='\u{312F}').contains(&c)
}

fn fold_case(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}
