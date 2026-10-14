//! N20a scrollback search core (headless).
//!
//! Literal (no regex) search over an owned snapshot of scrollback rows
//! (`&[Vec<char>]`, row 0 = oldest). A [`SearchState`] carries the
//! current query and match navigation; every search is tagged with the
//! state's generation so a result that completes after a newer query has
//! arrived is [`SearchOutcome::Cancelled`] rather than applied (N20b
//! will run searches as background jobs against this core).
//!
//! The search is bounded to the provided snapshot — it never reaches
//! into the engine or into unbounded history.

/// A search query and its current state.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// The search query (literal text).
    pub query: String,
    /// The current match index (0-based).
    pub current: usize,
    /// The total number of matches found.
    pub total: usize,
    /// The generation tag for this search (increments on each new query).
    pub generation: u64,
}

impl SearchState {
    /// Create a new search state with an empty query.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            current: 0,
            total: 0,
            generation: 0,
        }
    }

    /// Set a new query and reset the match state.
    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.current = 0;
        self.total = 0;
        self.generation += 1;
    }

    /// Move to the next match.
    pub fn next(&mut self) {
        if self.total > 0 {
            self.current = (self.current + 1) % self.total;
        }
    }

    /// Move to the previous match.
    pub fn prev(&mut self) {
        if self.total > 0 {
            if self.current == 0 {
                self.current = self.total - 1;
            } else {
                self.current -= 1;
            }
        }
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

/// A search result (a match in the scrollback).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// The row index in the scrollback (0 = oldest).
    pub row: usize,
    /// The column index within the row.
    pub col: usize,
}

/// The result of a search operation.
#[derive(Debug)]
pub enum SearchOutcome {
    /// Search completed successfully.
    Found {
        /// The list of matches.
        matches: Vec<SearchResult>,
        /// The generation tag for this search.
        generation: u64,
    },
    /// Search was cancelled (stale generation).
    Cancelled,
    /// Search failed.
    Error(String),
}

/// Run a literal search over `snapshot` for the state's current query.
///
/// The result is tagged with the state's generation so a caller that
/// defers the work (N20b's background job) can discard it when a newer
/// query has superseded it.
pub fn run_search(snapshot: &[Vec<char>], state: &SearchState) -> SearchOutcome {
    let matches = search_snapshot(snapshot, &state.query);
    SearchOutcome::Found {
        matches,
        generation: state.generation,
    }
}

/// Re-check a completed search against the state's current generation.
///
/// A `Found` result whose tag no longer matches is stale — a newer query
/// arrived while the search was in flight — and is returned as
/// [`SearchOutcome::Cancelled`]; the caller must drop it without
/// touching the state. Current results pass through unchanged.
pub fn check_stale(outcome: SearchOutcome, state: &SearchState) -> SearchOutcome {
    match outcome {
        SearchOutcome::Found {
            matches,
            generation,
        } if generation == state.generation => SearchOutcome::Found {
            matches,
            generation,
        },
        SearchOutcome::Found { .. } => SearchOutcome::Cancelled,
        other => other,
    }
}

/// Search a snapshot for a literal query.
///
/// Returns a list of matches (row, col) where the query appears, in
/// row-then-column order. The search is bounded to the provided
/// snapshot (no unlimited history).
pub fn search_snapshot(snapshot: &[Vec<char>], query: &str) -> Vec<SearchResult> {
    if query.is_empty() {
        return Vec::new();
    }

    let query_chars: Vec<char> = query.chars().collect();
    let mut results = Vec::new();

    for (row_idx, row) in snapshot.iter().enumerate() {
        // Simple literal search: find all occurrences of query_chars in
        // the row.
        if row.len() >= query_chars.len() {
            for col_idx in 0..=(row.len() - query_chars.len()) {
                let match_found =
                    (0..query_chars.len()).all(|i| row[col_idx + i] == query_chars[i]);
                if match_found {
                    results.push(SearchResult {
                        row: row_idx,
                        col: col_idx,
                    });
                }
            }
        }
    }

    results
}
