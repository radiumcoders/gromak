//! Fuzzy matching for the launcher: is `query` a subsequence of a name, and
//! how good a match is it?
//!
//! Matches at the start of the name or of a word, consecutive runs and
//! prefixes score higher; gaps score lower. For each query character the
//! matcher prefers the next word start over the next plain occurrence, so
//! "vsc" lights up **V**isual **S**tudio **C**ode rather than the first "s".

/// A successful match: a score (higher is better) and the matched character
/// positions (char indices into the candidate), for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    pub positions: Vec<usize>,
}

fn is_word_start(chars: &[char], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    let (prev, cur) = (chars[i - 1], chars[i]);
    !prev.is_alphanumeric() || (prev.is_lowercase() && cur.is_uppercase()) || (!prev.is_numeric() && cur.is_numeric())
}

/// Scores `candidate` against `query` (case-insensitive). An empty query
/// matches everything with score 0.
pub fn score(query: &str, candidate: &str) -> Option<Match> {
    let query: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect();
    if query.is_empty() {
        return Some(Match { score: 0, positions: Vec::new() });
    }
    let chars: Vec<char> = candidate.chars().collect();
    let lower: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();

    // latest[k]: the last position query[k] can take while query[k+1..] can
    // still be matched after it. Picks beyond it would make the match fail.
    let mut latest = vec![0usize; query.len()];
    let mut limit = lower.len();
    for k in (0..query.len()).rev() {
        latest[k] = (0..limit).rev().find(|&i| lower[i] == query[k])?;
        limit = latest[k];
    }

    let mut positions = Vec::with_capacity(query.len());
    let mut from = 0;
    for (k, &q) in query.iter().enumerate() {
        let rest = from..=latest[k];
        // Prefer continuing a run, then a word start, then any occurrence,
        // among positions that keep the rest of the query matchable.
        let continues = positions.last().is_some_and(|&p: &usize| p + 1 == from)
            && from <= latest[k]
            && lower.get(from) == Some(&q);
        let pick = if continues {
            Some(from)
        } else {
            rest.clone()
                .find(|&i| lower[i] == q && is_word_start(&chars, i))
                .or_else(|| rest.clone().find(|&i| lower[i] == q))
        }?;
        positions.push(pick);
        from = pick + 1;
    }

    let mut score = 0;
    for (k, &p) in positions.iter().enumerate() {
        score += 10;
        if is_word_start(&chars, p) {
            score += 15;
        }
        if p == 0 {
            score += 20;
        }
        if k > 0 {
            let gap = p - positions[k - 1] - 1;
            if gap == 0 {
                score += 8;
            } else {
                score -= gap.min(10) as i32;
            }
        }
    }
    let name_lower: String = lower.iter().collect();
    let query_str: String = query.iter().collect();
    if name_lower.starts_with(&query_str) {
        score += 40;
    }
    // Among equal matches, prefer shorter names.
    score -= (chars.len() / 8) as i32;
    Some(Match { score, positions })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rank<'a>(query: &str, names: &[&'a str]) -> Vec<&'a str> {
        let mut scored: Vec<(i32, &str)> =
            names.iter().filter_map(|n| score(query, n).map(|m| (m.score, *n))).collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        scored.into_iter().map(|(_, n)| n).collect()
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(score("", "Anything"), Some(Match { score: 0, positions: vec![] }));
    }

    #[test]
    fn requires_a_subsequence() {
        assert!(score("xyz", "Visual Studio Code").is_none());
        assert!(score("edoc", "Code").is_none());
        assert!(score("vsc", "Visual Studio Code").is_some());
    }

    #[test]
    fn is_case_insensitive_and_ignores_spaces() {
        assert!(score("FIREFOX", "Firefox").is_some());
        assert!(score("win term", "Windows Terminal").is_some());
    }

    #[test]
    fn prefers_word_starts_for_highlighting() {
        let m = score("vsc", "Visual Studio Code").unwrap();
        assert_eq!(m.positions, vec![0, 7, 14]);
    }

    #[test]
    fn word_start_preference_never_breaks_a_match() {
        // Jumping to the "S" of "Studio" would leave no "ual" after it.
        assert!(score("sual", "Visual Studio Code").is_some());
        assert!(score("sks", "Task Scheduler").is_some());
        assert_eq!(score("sual", "Visual Studio Code").unwrap().positions, vec![2, 3, 4, 5]);
    }

    #[test]
    fn camel_case_humps_are_word_starts() {
        let m = score("pt", "PowerToys").unwrap();
        assert_eq!(m.positions, vec![0, 5]);
    }

    #[test]
    fn prefix_and_initials_beat_scattered_matches() {
        assert_eq!(rank("term", &["Character Map", "Terminal", "Windows Terminal"])[0], "Terminal");
        assert_eq!(rank("vsc", &["Services", "Visual Studio Code"])[0], "Visual Studio Code");
        assert_eq!(rank("note", &["OneNote", "Notepad", "Sticky Notes"])[0], "Notepad");
    }

    #[test]
    fn handles_non_ascii_names() {
        let m = score("ca", "Café Après").unwrap();
        assert_eq!(m.positions, vec![0, 1]);
        assert!(score("é", "Café").is_some());
    }
}
