//! BM25 keyword search over memory names and content, backed by Tantivy.
//!
//! The lexical index is held entirely in RAM and rebuilt from the git repo
//! on startup (indexing text is cheap, unlike embedding it), so there is no
//! on-disk index to version or migrate. Writes go through the same handlers
//! that update the vector index, keeping the two views consistent.

use std::sync::Mutex;

use tantivy::{
    collector::TopDocs,
    query::{BooleanQuery, Occur, PhraseQuery, Query, QueryParser},
    schema::{Field, Schema, Value, STORED, STRING, TEXT},
    Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term,
};
use tracing::{debug, warn};

use crate::{
    error::MemoryError,
    types::{parse_qualified_name, ScopeFilter},
};

/// Heap budget for the single Tantivy writer thread. Tantivy requires at
/// least 15 MB; the corpus (markdown memories) is far smaller than this.
const WRITER_HEAP_BYTES: usize = 32 * 1024 * 1024;

/// Upper bound on candidates collected per query before scope filtering.
/// Scope filtering happens post-search, so a filtered query must be able to
/// look past out-of-scope matches; this caps the worst case.
const MAX_CANDIDATES: u64 = 10_000;

/// One memory's searchable text, as fed to [`LexicalIndex::rebuild`].
#[derive(Debug, Clone)]
pub struct LexicalDoc {
    /// Canonical index key (`v1:scope=<scope>;name=<name>`).
    pub qualified_name: String,
    /// Bare memory name (searchable).
    pub name: String,
    /// Full memory content (searchable).
    pub content: String,
}

/// In-RAM BM25 index over memory names and content.
///
/// Thread-safe: reads go through a Tantivy [`IndexReader`], writes are
/// serialised by an internal mutex. Every write commits and reloads the
/// reader so results are immediately visible — acceptable because writes
/// happen at memory-mutation frequency, not query frequency.
///
/// Construction is infallible: if the underlying Tantivy index cannot be
/// initialised (practically impossible for a RAM directory), the instance
/// is *disabled* — every operation returns an error, which callers already
/// treat as best-effort degradation to semantic-only retrieval.
pub struct LexicalIndex {
    inner: Option<Inner>,
}

struct Inner {
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    field_qualified_name: Field,
    field_name: Field,
    field_content: Field,
}

impl Inner {
    fn build() -> Result<Self, MemoryError> {
        let mut schema_builder = Schema::builder();
        // Raw (untokenised) unique key — used for deletes and result lookup.
        let field_qualified_name = schema_builder.add_text_field("qualified_name", STRING | STORED);
        // Tokenised searchable fields (default tokenizer: simple + lowercase).
        let field_name = schema_builder.add_text_field("name", TEXT);
        let field_content = schema_builder.add_text_field("content", TEXT);
        let schema = schema_builder.build();

        let index = Index::create_in_ram(schema);
        let writer = index
            .writer_with_num_threads(1, WRITER_HEAP_BYTES)
            .map_err(lexical_error)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(lexical_error)?;

        Ok(Self {
            index,
            writer: Mutex::new(writer),
            reader,
            field_qualified_name,
            field_name,
            field_content,
        })
    }

    fn lock_writer(&self) -> Result<std::sync::MutexGuard<'_, IndexWriter>, MemoryError> {
        self.writer
            .lock()
            .map_err(|_| MemoryError::Index("lexical index writer lock poisoned".to_string()))
    }
}

impl Default for LexicalIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl LexicalIndex {
    /// Create an empty in-RAM lexical index.
    ///
    /// Infallible by design: an initialisation failure produces a disabled
    /// instance whose operations all error, degrading recall to
    /// semantic-only rather than preventing startup.
    pub fn new() -> Self {
        let inner = match Inner::build() {
            Ok(inner) => Some(inner),
            Err(e) => {
                tracing::error!(
                    error = %e,
                    "lexical index initialisation failed — keyword search disabled"
                );
                None
            }
        };
        Self { inner }
    }

    #[cfg(test)]
    fn disabled() -> Self {
        Self { inner: None }
    }

    fn inner(&self) -> Result<&Inner, MemoryError> {
        self.inner.as_ref().ok_or_else(|| {
            MemoryError::Index("lexical index unavailable (initialisation failed)".to_string())
        })
    }

    /// Insert or replace the document for `qualified_name`.
    pub fn upsert(
        &self,
        qualified_name: &str,
        name: &str,
        content: &str,
    ) -> Result<(), MemoryError> {
        let span = tracing::debug_span!("lexical.upsert", qualified_name = %qualified_name);
        let _guard = span.enter();

        let inner = self.inner()?;
        let mut writer = inner.lock_writer()?;
        writer.delete_term(Term::from_field_text(
            inner.field_qualified_name,
            qualified_name,
        ));
        let mut doc = TantivyDocument::default();
        doc.add_text(inner.field_qualified_name, qualified_name);
        doc.add_text(inner.field_name, name);
        doc.add_text(inner.field_content, content);
        writer.add_document(doc).map_err(lexical_error)?;
        writer.commit().map_err(lexical_error)?;
        drop(writer);
        inner.reader.reload().map_err(lexical_error)
    }

    /// Remove the document for `qualified_name`. No-op if absent.
    pub fn remove(&self, qualified_name: &str) -> Result<(), MemoryError> {
        let span = tracing::debug_span!("lexical.remove", qualified_name = %qualified_name);
        let _guard = span.enter();

        let inner = self.inner()?;
        let mut writer = inner.lock_writer()?;
        writer.delete_term(Term::from_field_text(
            inner.field_qualified_name,
            qualified_name,
        ));
        writer.commit().map_err(lexical_error)?;
        drop(writer);
        inner.reader.reload().map_err(lexical_error)
    }

    /// Replace the entire index contents with `docs` in a single commit.
    pub fn rebuild<I>(&self, docs: I) -> Result<usize, MemoryError>
    where
        I: IntoIterator<Item = LexicalDoc>,
    {
        let span = tracing::debug_span!("lexical.rebuild", count = tracing::field::Empty);
        let _guard = span.enter();

        let inner = self.inner()?;
        let mut writer = inner.lock_writer()?;
        writer.delete_all_documents().map_err(lexical_error)?;
        let mut count = 0usize;
        for item in docs {
            let mut doc = TantivyDocument::default();
            doc.add_text(inner.field_qualified_name, &item.qualified_name);
            doc.add_text(inner.field_name, &item.name);
            doc.add_text(inner.field_content, &item.content);
            writer.add_document(doc).map_err(lexical_error)?;
            count += 1;
        }
        writer.commit().map_err(lexical_error)?;
        drop(writer);
        inner.reader.reload().map_err(lexical_error)?;
        tracing::Span::current().record("count", count);
        debug!(count, "lexical index rebuilt");
        Ok(count)
    }

    /// Run a BM25 keyword query, returning up to `limit`
    /// `(qualified_name, score)` pairs.
    ///
    /// Ranking is two-pass: documents matching the query as an *exact
    /// phrase* rank strictly above term-only matches. Plain BM25 term
    /// scoring lets a short document containing one query term outrank a
    /// long document containing the exact phrase (length normalisation
    /// dilutes the long document's term scores) — the failure mode behind
    /// #55. Within each pass, results are ordered by descending BM25 score
    /// with ties broken by ascending name, so the ranking is fully
    /// deterministic. Returned scores are only comparable within a pass;
    /// downstream fusion consumes ranks, not scores.
    ///
    /// Scope filtering matches recall semantics exactly: candidate scopes
    /// are parsed from the canonical key and checked with
    /// [`ScopeFilter::matches`].
    pub fn search(
        &self,
        filter: &ScopeFilter,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, f32)>, MemoryError> {
        let span = tracing::debug_span!("lexical.search", ?filter, limit);
        let _guard = span.enter();

        let inner = self.inner()?;
        if limit == 0 {
            return Ok(Vec::new());
        }
        let searcher = inner.reader.searcher();
        if searcher.num_docs() == 0 {
            return Ok(Vec::new());
        }

        let mut results: Vec<(String, f32)> = Vec::new();

        // Pass 1: exact-phrase matches (only meaningful for multi-term queries).
        if let Some(phrase_query) = inner.phrase_query(query) {
            let phrase_hits = inner.execute(&searcher, &phrase_query, filter, limit)?;
            results.extend(phrase_hits);
        }

        // Pass 2: regular term matching, skipping documents pass 1 found.
        let parser =
            QueryParser::for_index(&inner.index, vec![inner.field_name, inner.field_content]);
        // Lenient parsing: user queries are natural language, not query
        // syntax — unbalanced quotes/parens degrade instead of erroring.
        let (parsed, parse_errors) = parser.parse_query_lenient(query);
        if !parse_errors.is_empty() {
            debug!(errors = ?parse_errors, "lexical query parsed leniently");
        }
        let term_hits = inner.execute(&searcher, &parsed, filter, limit)?;
        for hit in term_hits {
            if !results.iter().any(|(name, _)| name == &hit.0) {
                results.push(hit);
            }
        }

        results.truncate(limit);
        Ok(results)
    }
}

impl Inner {
    /// Build a phrase query matching the whole `query` as an exact phrase
    /// in either the name or content field. Returns `None` when the query
    /// tokenises to fewer than two terms (a phrase needs at least two).
    fn phrase_query(&self, query: &str) -> Option<Box<dyn Query>> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for field in [self.field_name, self.field_content] {
            let Ok(mut analyzer) = self.index.tokenizer_for_field(field) else {
                // Unreachable for TEXT fields with the default tokenizer;
                // skip this field's clause rather than the whole phrase pass.
                continue;
            };
            let mut terms: Vec<Term> = Vec::new();
            let mut stream = analyzer.token_stream(query);
            while let Some(token) = stream.next() {
                terms.push(Term::from_field_text(field, &token.text));
            }
            if terms.len() >= 2 {
                clauses.push((Occur::Should, Box::new(PhraseQuery::new(terms))));
            }
        }
        if clauses.is_empty() {
            None
        } else {
            Some(Box::new(BooleanQuery::new(clauses)))
        }
    }

    /// Execute `query`, post-filter by scope, and return up to `limit`
    /// `(qualified_name, score)` pairs ordered by descending score with
    /// ties broken by ascending name.
    fn execute(
        &self,
        searcher: &Searcher,
        query: &dyn Query,
        filter: &ScopeFilter,
        limit: usize,
    ) -> Result<Vec<(String, f32)>, MemoryError> {
        // Scope filtering happens after search, so collect enough candidates
        // to look past out-of-scope matches (bounded by MAX_CANDIDATES).
        let fetch = usize::try_from(searcher.num_docs().min(MAX_CANDIDATES))
            .unwrap_or(usize::MAX)
            .max(limit);
        let top_docs = searcher
            .search(query, &TopDocs::with_limit(fetch).order_by_score())
            .map_err(lexical_error)?;

        let mut results: Vec<(String, f32)> = Vec::new();
        for (score, address) in top_docs {
            let doc: TantivyDocument = searcher.doc(address).map_err(lexical_error)?;
            let Some(qualified_name) = doc
                .get_first(self.field_qualified_name)
                .and_then(|v| v.as_str())
            else {
                warn!("lexical index document is missing its qualified_name; skipping");
                continue;
            };
            let mref = match parse_qualified_name(qualified_name) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        qualified_name = %qualified_name,
                        error = %e,
                        "could not parse qualified name from lexical index; skipping"
                    );
                    continue;
                }
            };
            if filter.matches(&mref.scope) {
                results.push((qualified_name.to_string(), score));
            }
        }

        results.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        results.truncate(limit);
        Ok(results)
    }
}

fn lexical_error(e: tantivy::TantivyError) -> MemoryError {
    MemoryError::Index(format!("lexical index: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MemoryName, MemoryRef, Scope, ScopePath};

    fn key(scope: &Scope, name: &str) -> String {
        MemoryRef::new(scope.clone(), MemoryName::new(name.to_string()).unwrap()).qualified_path()
    }

    fn scoped(path: &str) -> Scope {
        Scope::Path(ScopePath::new(path).unwrap())
    }

    fn subtree(path: &str) -> ScopeFilter {
        ScopeFilter::Subtree(ScopePath::new(path).unwrap())
    }

    fn index_with(docs: &[(&Scope, &str, &str)]) -> LexicalIndex {
        let index = LexicalIndex::new();
        for (scope, name, content) in docs {
            index
                .upsert(&key(scope, name), name, content)
                .expect("upsert");
        }
        index
    }

    #[test]
    fn keyword_match_is_found_and_scored() {
        let index = index_with(&[
            (
                &Scope::Root,
                "greeting",
                "happy birthday is a morning ritual",
            ),
            (&Scope::Root, "other", "kubernetes deployment configuration"),
        ]);

        let results = index
            .search(&ScopeFilter::All, "happy birthday", 10)
            .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, key(&Scope::Root, "greeting"));
        assert!(results[0].1 > 0.0, "BM25 score should be positive");
    }

    #[test]
    fn memory_name_is_searchable() {
        let index = index_with(&[(&Scope::Root, "birthday-protocol", "unrelated body text")]);

        let results = index
            .search(&ScopeFilter::All, "birthday", 10)
            .expect("search");

        assert_eq!(results.len(), 1);
    }

    #[test]
    fn upsert_replaces_previous_document() {
        let scope = Scope::Root;
        let index = index_with(&[(&scope, "note", "contains magicword here")]);

        index
            .upsert(&key(&scope, "note"), "note", "completely different now")
            .expect("upsert");

        let old = index
            .search(&ScopeFilter::All, "magicword", 10)
            .expect("search");
        assert!(old.is_empty(), "old content should no longer match");

        let new = index
            .search(&ScopeFilter::All, "different", 10)
            .expect("search");
        assert_eq!(new.len(), 1, "new content should match exactly once");
    }

    #[test]
    fn remove_makes_document_unreachable() {
        let scope = Scope::Root;
        let index = index_with(&[(&scope, "doomed", "contains magicword here")]);

        index.remove(&key(&scope, "doomed")).expect("remove");

        let results = index
            .search(&ScopeFilter::All, "magicword", 10)
            .expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn scope_filter_root_only_excludes_scoped_memories() {
        let proj = scoped("proj");
        let index = index_with(&[
            (&Scope::Root, "root-note", "magicword in global"),
            (&proj, "proj-note", "magicword in project"),
        ]);

        let results = index
            .search(&ScopeFilter::RootOnly, "magicword", 10)
            .expect("search");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, key(&Scope::Root, "root-note"));
    }

    #[test]
    fn scope_filter_subtree_includes_root_and_descendants_only() {
        let team = scoped("org/team");
        let nested = scoped("org/team/project");
        let other = scoped("org/other");
        let index = index_with(&[
            (&Scope::Root, "root-note", "magicword in global"),
            (&team, "team-note", "magicword in team"),
            (&nested, "nested-note", "magicword in nested"),
            (&other, "other-note", "magicword in other"),
        ]);

        let results = index
            .search(&subtree("org/team"), "magicword", 10)
            .expect("search");

        let names: Vec<&str> = results.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(results.len(), 3);
        assert!(names.contains(&key(&Scope::Root, "root-note").as_str()));
        assert!(names.contains(&key(&team, "team-note").as_str()));
        assert!(names.contains(&key(&nested, "nested-note").as_str()));
    }

    #[test]
    fn scope_filter_all_includes_everything() {
        let proj = scoped("proj");
        let index = index_with(&[
            (&Scope::Root, "root-note", "magicword in global"),
            (&proj, "proj-note", "magicword in project"),
        ]);

        let results = index
            .search(&ScopeFilter::All, "magicword", 10)
            .expect("search");

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let index = index_with(&[(&Scope::Root, "note", "Happy Birthday To You")]);

        let results = index
            .search(&ScopeFilter::All, "happy birthday", 10)
            .expect("search");

        assert_eq!(results.len(), 1);
    }

    #[test]
    fn query_syntax_errors_degrade_instead_of_failing() {
        let index = index_with(&[(&Scope::Root, "note", "unbalanced quote content")]);

        // Unbalanced quotes and parens must not produce an error.
        let result = index.search(&ScopeFilter::All, "\"unbalanced (quote", 10);
        assert!(result.is_ok(), "lenient parsing should never error");
    }

    #[test]
    fn empty_index_and_zero_limit_return_empty() {
        let index = LexicalIndex::new();
        assert!(index
            .search(&ScopeFilter::All, "anything", 10)
            .expect("search")
            .is_empty());

        index
            .upsert(&key(&Scope::Root, "note"), "note", "content")
            .expect("upsert");
        assert!(index
            .search(&ScopeFilter::All, "content", 0)
            .expect("search")
            .is_empty());
    }

    #[test]
    fn rebuild_replaces_all_contents() {
        let scope = Scope::Root;
        let index = index_with(&[(&scope, "stale", "oldword content")]);

        let count = index
            .rebuild(vec![
                LexicalDoc {
                    qualified_name: key(&scope, "fresh-a"),
                    name: "fresh-a".to_string(),
                    content: "newword alpha".to_string(),
                },
                LexicalDoc {
                    qualified_name: key(&scope, "fresh-b"),
                    name: "fresh-b".to_string(),
                    content: "newword beta".to_string(),
                },
            ])
            .expect("rebuild");
        assert_eq!(count, 2);

        assert!(index
            .search(&ScopeFilter::All, "oldword", 10)
            .expect("search")
            .is_empty());
        assert_eq!(
            index
                .search(&ScopeFilter::All, "newword", 10)
                .expect("search")
                .len(),
            2
        );
    }

    #[test]
    fn exact_phrase_in_long_document_outranks_short_term_only_matches() {
        let scope = Scope::Root;
        // Long document: the exact phrase is buried in unrelated prose, so
        // BM25 length normalisation dilutes its per-term scores.
        let mut long_content = String::new();
        for i in 0..80 {
            long_content.push_str(&format!(
                "Filler sentence number {i} about infrastructure, deployments, and rust. "
            ));
        }
        long_content.push_str("the magic phrase appears exactly once here. ");
        for i in 0..80 {
            long_content.push_str(&format!("More filler prose {i} about unrelated topics. "));
        }

        let index = index_with(&[
            (&scope, "long-doc", long_content.as_str()),
            (&scope, "decoy-a", "magic tricks for beginners"),
            (&scope, "decoy-b", "phrase books for travellers"),
            (
                &scope,
                "decoy-c",
                "magic and phrase collections, magic themed",
            ),
        ]);

        let results = index
            .search(&ScopeFilter::All, "magic phrase", 10)
            .expect("search");

        assert_eq!(
            results[0].0,
            key(&scope, "long-doc"),
            "exact-phrase match must rank above term-only matches: {results:?}"
        );
    }

    #[test]
    fn disabled_instance_errors_on_every_operation() {
        let index = LexicalIndex::disabled();

        assert!(index.upsert("key", "name", "content").is_err());
        assert!(index.remove("key").is_err());
        assert!(index.rebuild(Vec::new()).is_err());
        assert!(index.search(&ScopeFilter::All, "query", 10).is_err());
    }

    #[test]
    fn results_are_deterministically_ordered() {
        let scope = Scope::Root;
        // Two documents with identical content produce identical BM25
        // scores — ordering must fall back to the qualified name.
        let index = index_with(&[
            (&scope, "zeta", "identical magicword content"),
            (&scope, "alpha", "identical magicword content"),
        ]);

        let results = index
            .search(&ScopeFilter::All, "magicword", 10)
            .expect("search");

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, key(&scope, "alpha"));
        assert_eq!(results[1].0, key(&scope, "zeta"));
    }
}
