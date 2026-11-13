//! Query words and IDF weights for ranked search.
//!
//! The rules match the LoCoMo research harness, so its numbers carry over.

/// Common English function words, question words and light verbs. Words under 3 chars are
/// dropped anyway. Copied from `STOPWORDS` in `research/locomo_retrieval/words.py`. It is
/// fixed, and never tuned on answers.
const STOPWORDS: [&str; 159] = [
    "about",
    "above",
    "after",
    "again",
    "against",
    "all",
    "also",
    "although",
    "among",
    "and",
    "another",
    "any",
    "anyone",
    "anything",
    "are",
    "around",
    "because",
    "been",
    "before",
    "being",
    "below",
    "between",
    "both",
    "but",
    "can",
    "could",
    "did",
    "does",
    "doing",
    "done",
    "down",
    "during",
    "each",
    "either",
    "else",
    "even",
    "ever",
    "every",
    "few",
    "for",
    "from",
    "further",
    "get",
    "gets",
    "got",
    "had",
    "has",
    "have",
    "having",
    "her",
    "here",
    "hers",
    "herself",
    "him",
    "himself",
    "his",
    "how",
    "however",
    "into",
    "its",
    "itself",
    "just",
    "least",
    "less",
    "let",
    "like",
    "made",
    "make",
    "many",
    "may",
    "might",
    "more",
    "most",
    "much",
    "must",
    "myself",
    "near",
    "neither",
    "nor",
    "not",
    "now",
    "off",
    "once",
    "one",
    "only",
    "onto",
    "other",
    "others",
    "our",
    "ours",
    "ourselves",
    "out",
    "over",
    "own",
    "per",
    "quite",
    "rather",
    "really",
    "same",
    "say",
    "said",
    "she",
    "should",
    "since",
    "some",
    "something",
    "still",
    "such",
    "than",
    "that",
    "the",
    "their",
    "theirs",
    "them",
    "themselves",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "things",
    "this",
    "those",
    "though",
    "through",
    "thus",
    "together",
    "too",
    "toward",
    "towards",
    "under",
    "until",
    "upon",
    "very",
    "was",
    "were",
    "what",
    "whatever",
    "when",
    "whenever",
    "where",
    "whether",
    "which",
    "while",
    "who",
    "whom",
    "whose",
    "why",
    "will",
    "with",
    "within",
    "without",
    "would",
    "yet",
    "you",
    "your",
    "yours",
    "yourself",
    "yourselves",
];

/// Words shorter than this many chars are stopwords too.
const MIN_CHARS: usize = 3;

/// The words ranked search scores, in first-seen order without repeats. The query is
/// lowercased and split on every char that isn't alphanumeric. Words of 3 or more chars that
/// aren't stopwords are kept. If none are, every word is kept, so `is it ok` still matches.
pub(crate) fn query_words(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let mut all: Vec<&str> = Vec::new();
    for word in lower.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()) {
        if !all.contains(&word) {
            all.push(word);
        }
    }
    let content: Vec<&str> = all
        .iter()
        .copied()
        .filter(|word| word.chars().count() >= MIN_CHARS && !STOPWORDS.contains(word))
        .collect();
    let words = if content.is_empty() { all } else { content };
    words.into_iter().map(str::to_owned).collect()
}

/// The weight of a word that `df` of `n` records hold. Rare words weigh more. It is BM25's
/// IDF, `ln(1 + (n - df + 0.5) / (df + 0.5))`, which stays above 0.
pub(crate) fn idf(n: u64, df: u64) -> f64 {
    let (n, df) = (n as f64, df as f64);
    (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_words_drop_stopwords_and_short_words() {
        assert_eq!(query_words("What did Caroline paint, in 2022?"), ["caroline", "paint", "2022"]);
        assert_eq!(query_words("is it ok"), ["is", "it", "ok"], "nothing left, so every word");
    }

    #[test]
    fn rare_words_weigh_more() {
        assert!(idf(100, 1) > idf(100, 50));
    }
}
