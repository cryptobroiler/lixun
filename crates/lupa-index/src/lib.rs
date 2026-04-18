//! Lupa Index — Tantivy wrapper: schema, writer, searcher.

use anyhow::Result;
use std::path::Path;
use tantivy::{
    Index, IndexWriter, TantivyDocument, Term,
    collector::TopDocs,
    directory::MmapDirectory,
    doc,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query as TQuery},
    schema::{STORED, Schema, TEXT, Value},
};

use lupa_core::{Category, Document, Hit, Query};

/// Tantivy schema fields.
pub struct LupaSchema {
    pub schema: Schema,
    pub id: tantivy::schema::Field,
    pub category: tantivy::schema::Field,
    pub title: tantivy::schema::Field,
    pub subtitle: tantivy::schema::Field,
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

        let id = builder.add_text_field("id", TEXT | STORED);
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

        Self {
            schema,
            id,
            category,
            title,
            subtitle,
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

        let index = if Path::new(index_path).join("meta.json").exists() {
            Index::open_in_dir(index_path)?
        } else {
            std::fs::create_dir_all(index_path)?;
            let dir = MmapDirectory::open(index_path)?;
            Index::create(
                dir,
                schema.schema.clone(),
                tantivy::IndexSettings::default(),
            )?
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

        // Delete old
        let term = tantivy::Term::from_field_text(s.id, &doc.id.0);
        writer.delete_term(term);

        // Insert
        let action_json = serde_json::to_string(&doc.action)?;

        writer.add_document(doc![
            s.id => doc.id.0.as_str(),
            s.category => doc.category.as_str(),
            s.title => doc.title.as_str(),
            s.subtitle => doc.subtitle.as_str(),
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
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();

            let category_str = doc
                .get_first(s.category)
                .and_then(|v| v.as_str())
                .unwrap_or("file");

            let category = match category_str {
                "app" => Category::App,
                "file" => Category::File,
                "mail" => Category::Mail,
                "attachment" => Category::Attachment,
                _ => Category::File,
            };

            let title = doc
                .get_first(s.title)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let subtitle = doc
                .get_first(s.subtitle)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let action_json = doc
                .get_first(s.action)
                .and_then(|v| v.as_str())
                .unwrap_or("{}");

            let action: lupa_core::Action = serde_json::from_str(action_json)
                .unwrap_or(lupa_core::Action::OpenFile { path: "".into() });

            let extract_fail = doc
                .get_first(s.extract_fail)
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            // Apply category boost
            let boosted_score = score * category.ranking_boost();

            results.push(Hit {
                id: lupa_core::DocId(id),
                category,
                title,
                subtitle,
                score: boosted_score,
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

fn build_fuzzy_query(text: &str, s: &LupaSchema) -> Box<dyn TQuery> {
    let terms: Vec<&str> = text.split_whitespace().collect();
    if terms.is_empty() {
        return Box::new(BooleanQuery::new(vec![]));
    }

    let mut subqueries: Vec<(Occur, Box<dyn TQuery>)> = Vec::new();

    for term in &terms {
        let distance = if term.len() <= 5 { 1u8 } else { 2u8 };

        for field in [s.title, s.body, s.path] {
            let t = Term::from_field_text(field, term);
            let fq = FuzzyTermQuery::new(t, distance, true);
            subqueries.push((Occur::Should, Box::new(fq)));
        }
    }

    if subqueries.len() == 1 {
        subqueries.pop().unwrap().1
    } else {
        Box::new(BooleanQuery::new(subqueries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_search() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();

        let doc = Document {
            id: lupa_core::DocId("fs:/tmp/test.txt".to_string()),
            category: Category::File,
            title: "test.txt".to_string(),
            subtitle: "/tmp/test.txt".to_string(),
            body: Some("hello world this is test content".to_string()),
            path: "/tmp/test.txt".to_string(),
            mtime: 0,
            size: 100,
            action: lupa_core::Action::OpenFile {
                path: "/tmp/test.txt".into(),
            },
            extract_fail: false,
        };

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
    }

    #[test]
    fn test_delete_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();

        let doc = Document {
            id: lupa_core::DocId("fs:/tmp/delete_me.txt".to_string()),
            category: Category::File,
            title: "delete_me.txt".to_string(),
            subtitle: "/tmp/delete_me.txt".to_string(),
            body: Some("this will be deleted".to_string()),
            path: "/tmp/delete_me.txt".to_string(),
            mtime: 0,
            size: 100,
            action: lupa_core::Action::OpenFile {
                path: "/tmp/delete_me.txt".into(),
            },
            extract_fail: false,
        };

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
    }

    #[test]
    fn test_upsert_replaces_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        let mut index = LupaIndex::create_or_open(path).unwrap();
        let mut writer = index.writer(20_000_000).unwrap();

        let doc1 = Document {
            id: lupa_core::DocId("fs:/tmp/same.txt".to_string()),
            category: Category::File,
            title: "old_title.txt".to_string(),
            subtitle: "/tmp/same.txt".to_string(),
            body: Some("old content".to_string()),
            path: "/tmp/same.txt".to_string(),
            mtime: 100,
            size: 100,
            action: lupa_core::Action::OpenFile {
                path: "/tmp/same.txt".into(),
            },
            extract_fail: false,
        };

        index.upsert(&doc1, &mut writer).unwrap();

        let doc2 = Document {
            id: lupa_core::DocId("fs:/tmp/same.txt".to_string()),
            category: Category::File,
            title: "new_title.txt".to_string(),
            subtitle: "/tmp/same.txt".to_string(),
            body: Some("new content".to_string()),
            path: "/tmp/same.txt".to_string(),
            mtime: 200,
            size: 200,
            action: lupa_core::Action::OpenFile {
                path: "/tmp/same.txt".into(),
            },
            extract_fail: false,
        };

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
            let doc = Document {
                id: lupa_core::DocId(format!("fs:/tmp/doc{}.txt", i)),
                category: Category::File,
                title: format!("doc{}.txt", i),
                subtitle: format!("/tmp/doc{}.txt", i),
                body: Some(format!("content number {}", i)),
                path: format!("/tmp/doc{}.txt", i),
                mtime: i as i64,
                size: 100,
                action: lupa_core::Action::OpenFile {
                    path: format!("/tmp/doc{}.txt", i).into(),
                },
                extract_fail: false,
            };
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
            let doc = Document {
                id: lupa_core::DocId(format!("fs:/tmp/lim{}.txt", i)),
                category: Category::File,
                title: format!("lim{}.txt", i),
                subtitle: format!("/tmp/lim{}.txt", i),
                body: Some(format!("limit test {}", i)),
                path: format!("/tmp/lim{}.txt", i),
                mtime: i as i64,
                size: 100,
                action: lupa_core::Action::OpenFile {
                    path: format!("/tmp/lim{}.txt", i).into(),
                },
                extract_fail: false,
            };
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
}
