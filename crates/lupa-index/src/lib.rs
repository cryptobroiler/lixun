//! Lupa Index — Tantivy wrapper: schema, writer, searcher.

use anyhow::Result;
use std::path::{Path, PathBuf};
use tantivy::{
    collector::TopDocs,
    directory::MmapDirectory,
    doc,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query as TQuery},
    schema::{Schema, Value, STORED, STRING, TEXT},
    Index, IndexWriter, TantivyDocument, Term,
};

use lupa_core::{Category, Document, Hit, Query};

const INDEX_VERSION: u32 = 2;
const INDEX_VERSION_FILE: &str = "index_version.txt";

/// Tantivy schema fields.
pub struct LupaSchema {
    pub schema: Schema,
    pub id: tantivy::schema::Field,
    pub category: tantivy::schema::Field,
    pub title: tantivy::schema::Field,
    pub subtitle: tantivy::schema::Field,
    pub icon_name: tantivy::schema::Field,
    pub kind_label: tantivy::schema::Field,
    pub body: tantivy::schema::Field,
    pub path: tantivy::schema::Field,
    pub mtime: tantivy::schema::Field,
    pub size: tantivy::schema::Field,
    pub action: tantivy::schema::Field,
    pub extract_fail: tantivy::schema::Field,
}

impl LupaSchema {
    pub fn build() -> Self {
        let mut builder = tantivy::schema::Schema::builder();

        let id = builder.add_text_field("id", STRING | STORED);
        let category = builder.add_text_field("category", TEXT | STORED);
        let title = builder.add_text_field("title", TEXT | STORED);
        let subtitle = builder.add_text_field("subtitle", STORED);
        let icon_name = builder.add_text_field("icon_name", STORED);
        let kind_label = builder.add_text_field("kind_label", STORED);
        let body = builder.add_text_field("body", TEXT);
        let path = builder.add_text_field("path", TEXT | STORED);
        let mtime = builder.add_i64_field("mtime", STORED);
        let size = builder.add_u64_field("size", STORED);
        let action = builder.add_text_field("action", STORED);
        let extract_fail = builder.add_bool_field("extract_fail", STORED);

        let schema = builder.build();

        Self {
            schema,
            id,
            category,
            title,
            subtitle,
            icon_name,
            kind_label,
            body,
            path,
            mtime,
            size,
            action,
            extract_fail,
        }
    }
}

/// Index wrapper with search and upsert.
pub struct LupaIndex {
    index: Index,
    schema: LupaSchema,
}

impl LupaIndex {
    pub fn create_or_open(index_path: &str) -> Result<Self> {
        let schema = LupaSchema::build();
        let index_dir = PathBuf::from(index_path);
        let version_path = index_dir.join(INDEX_VERSION_FILE);
        let meta_path = index_dir.join("meta.json");

        let version_matches = read_index_version(&version_path) == Some(INDEX_VERSION);
        let has_meta = meta_path.exists();

        let index = if version_matches && has_meta {
            Index::open_in_dir(index_path)?
        } else {
            recreate_index_dir(&index_dir, read_index_version_string(&version_path))?;
            let dir = MmapDirectory::open(index_path)?;
            let index = Index::create(
                dir,
                schema.schema.clone(),
                tantivy::IndexSettings::default(),
            )?;
            std::fs::write(&version_path, INDEX_VERSION.to_string())?;
            index
        };

        Ok(Self { index, schema })
    }

    /// Upsert a document (delete by id, then insert).
    pub fn upsert(
        &mut self,
        doc: &Document,
        writer: &mut IndexWriter<TantivyDocument>,
    ) -> Result<()> {
        let s = &self.schema;

        let term = tantivy::Term::from_field_text(s.id, &doc.id.0);
        writer.delete_term(term);

        let action_json = serde_json::to_string(&doc.action)?;

        writer.add_document(doc![
            s.id => doc.id.0.as_str(),
            s.category => doc.category.as_str(),
            s.title => doc.title.as_str(),
            s.subtitle => doc.subtitle.as_str(),
            s.icon_name => doc.icon_name.as_deref().unwrap_or(""),
            s.kind_label => doc.kind_label.as_deref().unwrap_or(""),
            s.body => doc.body.as_deref().unwrap_or(""),
            s.path => doc.path.as_str(),
            s.mtime => doc.mtime,
            s.size => doc.size,
            s.action => action_json.as_str(),
            s.extract_fail => doc.extract_fail,
        ])?;

        Ok(())
    }

    /// Delete a document by id.
    pub fn delete_by_id(
        &mut self,
        id: &str,
        writer: &mut IndexWriter<TantivyDocument>,
    ) -> Result<()> {
        let term = tantivy::Term::from_field_text(self.schema.id, id);
        writer.delete_term(term);
        Ok(())
    }

    /// Search the index with fuzzy matching.
    pub fn search(&self, query: &Query) -> Result<Vec<Hit>> {
        let reader = self.index.reader()?;
        let searcher = reader.searcher();
        let s = &self.schema;

        let query_obj = build_fuzzy_query(&query.text, s);
        let top_docs = searcher.search(&query_obj, &TopDocs::with_limit(query.limit as usize))?;

        let mut results = Vec::new();
        for (score, doc_address) in top_docs {
            let doc: TantivyDocument = searcher.doc(doc_address)?;

            let id = doc
                .get_first(s.id)
                .and_then(|value| value.as_str())
                .unwrap_or("?")
                .to_string();

            let category = match doc
                .get_first(s.category)
                .and_then(|value| value.as_str())
                .unwrap_or("file")
            {
                "app" => Category::App,
                "file" => Category::File,
                "mail" => Category::Mail,
                "attachment" => Category::Attachment,
                _ => Category::File,
            };

            let title = doc
                .get_first(s.title)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();

            let subtitle = doc
                .get_first(s.subtitle)
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();

            let icon_name = stored_optional_text(&doc, s.icon_name);
            let kind_label = stored_optional_text(&doc, s.kind_label);

            let action_json = doc
                .get_first(s.action)
                .and_then(|value| value.as_str())
                .unwrap_or("{}");

            let action: lupa_core::Action = serde_json::from_str(action_json)
                .unwrap_or(lupa_core::Action::OpenFile { path: "".into() });

            let extract_fail = doc
                .get_first(s.extract_fail)
                .and_then(|value| value.as_bool())
                .unwrap_or(false);

            results.push(Hit {
                id: lupa_core::DocId(id),
                category: category.clone(),
                title,
                subtitle,
                icon_name,
                kind_label,
                score: score * category.ranking_boost(),
                action,
                extract_fail,
            });
        }

        Ok(results)
    }

    /// Commit pending writes.
    pub fn commit(&mut self, writer: &mut IndexWriter<TantivyDocument>) -> Result<()> {
        writer.commit()?;
        Ok(())
    }

    /// Create an index writer.
    pub fn writer(&self, heap_size: usize) -> Result<IndexWriter<TantivyDocument>> {
        let writer = self.index.writer(heap_size)?;
        Ok(writer)
    }
}

fn stored_optional_text(doc: &TantivyDocument, field: tantivy::schema::Field) -> Option<String> {
    let text = doc
        .get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or("");

    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn read_index_version(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_index_version_string(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|contents| contents.trim().to_string())
        .filter(|contents| !contents.is_empty())
}

fn recreate_index_dir(index_dir: &Path, old_version: Option<String>) -> Result<()> {
    if index_dir.exists() {
        tracing::info!(
            path = %index_dir.display(),
            old_version = old_version.as_deref().unwrap_or("missing"),
            new_version = INDEX_VERSION,
            "Wiping stale index directory"
        );
        std::fs::remove_dir_all(index_dir)?;
    }

    std::fs::create_dir_all(index_dir)?;
    Ok(())
}

fn build_fuzzy_query(text: &str, s: &LupaSchema) -> Box<dyn TQuery> {
    let terms: Vec<&str> = text.split_whitespace().collect();
    if terms.is_empty() {
        return Box::new(BooleanQuery::new(vec![]));
    }

    let mut subqueries: Vec<(Occur, Box<dyn TQuery>)> = Vec::new();

    for term in &terms {
        let distance = if term.len() <= 5 { 1u8 } else { 2u8 };

        for field in [s.title, s.body, s.path] {
            let fq = FuzzyTermQuery::new(Term::from_field_text(field, term), distance, true);
            subqueries.push((Occur::Should, Box::new(fq)));
        }
    }

    if subqueries.len() == 1 {
        subqueries.pop().expect("single query").1
    } else {
        Box::new(BooleanQuery::new(subqueries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document(id: &str, title: &str, body: &str) -> Document {
        Document {
            id: lupa_core::DocId(id.to_string()),
            category: Category::File,
            title: title.to_string(),
            subtitle: id.to_string(),
            icon_name: None,
            kind_label: None,
            body: Some(body.to_string()),
            path: id.trim_start_matches("fs:").to_string(),
            mtime: 0,
            size: 100,
            action: lupa_core::Action::OpenFile {
                path: id.trim_start_matches("fs:").into(),
            },
            extract_fail: false,
        }
    }

    #[test]
    fn test_create_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();
        let doc = sample_document(
            "fs:/tmp/test.txt",
            "test.txt",
            "hello world this is test content",
        );

        index.upsert(&doc, &mut writer).unwrap();
        index.commit(&mut writer).unwrap();

        let results = index
            .search(&Query {
                text: "hello".to_string(),
                limit: 10,
            })
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "test.txt");
        assert_eq!(results[0].icon_name, None);
        assert_eq!(results[0].kind_label, None);
    }

    #[test]
    fn test_delete_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();
        let doc = sample_document(
            "fs:/tmp/delete_me.txt",
            "delete_me.txt",
            "this will be deleted",
        );

        index.upsert(&doc, &mut writer).unwrap();
        index.commit(&mut writer).unwrap();
        writer.wait_merging_threads().unwrap();

        let mut writer = index.writer(20_000_000).unwrap();
        index
            .delete_by_id("fs:/tmp/delete_me.txt", &mut writer)
            .unwrap();
        index
            .delete_by_id("fs:/nonexistent.txt", &mut writer)
            .unwrap();
        index.commit(&mut writer).unwrap();
        writer.wait_merging_threads().unwrap();

        let results = index
            .search(&Query {
                text: "deleted".to_string(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();

        let doc1 = sample_document("fs:/tmp/same.txt", "old_title.txt", "old content");
        let doc2 = sample_document("fs:/tmp/same.txt", "new_title.txt", "new content");

        index.upsert(&doc1, &mut writer).unwrap();
        index.upsert(&doc2, &mut writer).unwrap();
        index.commit(&mut writer).unwrap();

        let results = index
            .search(&Query {
                text: "new".to_string(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "new_title.txt");
    }

    #[test]
    fn test_empty_search_returns_no_results() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let index = LupaIndex::create_or_open(path).unwrap();

        let results = index
            .search(&Query {
                text: "".to_string(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_multiple_documents_search() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();

        for i in 0..5 {
            let doc = sample_document(
                &format!("fs:/tmp/doc{i}.txt"),
                &format!("doc{i}.txt"),
                &format!("content number {i}"),
            );
            index.upsert(&doc, &mut writer).unwrap();
        }
        index.commit(&mut writer).unwrap();

        let results = index
            .search(&Query {
                text: "content".to_string(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn test_search_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();

        for i in 0..10 {
            let doc = sample_document(
                &format!("fs:/tmp/lim{i}.txt"),
                &format!("lim{i}.txt"),
                &format!("limit test {i}"),
            );
            index.upsert(&doc, &mut writer).unwrap();
        }
        index.commit(&mut writer).unwrap();

        let results = index
            .search(&Query {
                text: "limit".to_string(),
                limit: 3,
            })
            .unwrap();
        assert!(results.len() <= 3);
    }

    #[test]
    fn test_upsert_and_search_round_trips_icon_and_kind() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();
        let mut doc = sample_document("fs:/tmp/roundtrip.pdf", "roundtrip.pdf", "pdf body");
        doc.icon_name = Some("application-pdf".into());
        doc.kind_label = Some("PDF Document".into());

        index.upsert(&doc, &mut writer).unwrap();
        index.commit(&mut writer).unwrap();

        let results = index
            .search(&Query {
                text: "pdf".to_string(),
                limit: 10,
            })
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].icon_name.as_deref(), Some("application-pdf"));
        assert_eq!(results[0].kind_label.as_deref(), Some("PDF Document"));
    }

    #[test]
    fn test_create_or_open_rebuilds_index_when_version_file_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let old_schema = {
            let mut builder = tantivy::schema::Schema::builder();
            let id = builder.add_text_field("id", STRING | STORED);
            let category = builder.add_text_field("category", TEXT | STORED);
            let title = builder.add_text_field("title", TEXT | STORED);
            let subtitle = builder.add_text_field("subtitle", STORED);
            let body = builder.add_text_field("body", TEXT);
            let path = builder.add_text_field("path", TEXT | STORED);
            let mtime = builder.add_i64_field("mtime", STORED);
            let size = builder.add_u64_field("size", STORED);
            let action = builder.add_text_field("action", STORED);
            let extract_fail = builder.add_bool_field("extract_fail", STORED);
            let schema = builder.build();

            let dir = MmapDirectory::open(tmp.path()).unwrap();
            let index =
                Index::create(dir, schema.clone(), tantivy::IndexSettings::default()).unwrap();
            let mut writer = index.writer::<TantivyDocument>(20_000_000).unwrap();
            writer
                .add_document(doc![
                    id => "fs:/tmp/legacy.txt",
                    category => "file",
                    title => "legacy.txt",
                    subtitle => "/tmp/legacy.txt",
                    body => "legacy body",
                    path => "/tmp/legacy.txt",
                    mtime => 0i64,
                    size => 1u64,
                    action => serde_json::to_string(&lupa_core::Action::OpenFile { path: "/tmp/legacy.txt".into() }).unwrap(),
                    extract_fail => false,
                ])
                .unwrap();
            writer.commit().unwrap();
            schema
        };
        assert!(tmp.path().join("meta.json").exists());
        assert!(!tmp.path().join(INDEX_VERSION_FILE).exists());
        drop(old_schema);

        let index = LupaIndex::create_or_open(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(INDEX_VERSION_FILE))
                .unwrap()
                .trim(),
            INDEX_VERSION.to_string()
        );

        let results = index
            .search(&Query {
                text: "legacy".to_string(),
                limit: 10,
            })
            .unwrap();
        assert!(results.is_empty());
    }
}
