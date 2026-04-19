use anyhow::Result;
use lupa_core::{Document, PluginFieldSpec};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct SourceContext<'a> {
    pub instance_id: &'a str,
    pub state_dir: &'a Path,
}

pub struct WatchSpec {
    pub path: PathBuf,
    pub recursive: bool,
}

#[derive(Clone, Debug)]
pub struct SourceEvent {
    pub path: PathBuf,
    pub kind: SourceEventKind,
}

#[derive(Clone, Debug)]
pub enum SourceEventKind {
    Created,
    Modified,
    Removed,
    Renamed { from: PathBuf },
}

pub enum Mutation {
    Upsert(Box<Document>),
    Delete { doc_id: String },
    DeleteSourceInstance { instance_id: String },
}

pub trait MutationSink: Send + Sync {
    fn emit(&self, mutation: Mutation) -> Result<()>;
}

pub trait IndexerSource: Send + Sync {
    fn kind(&self) -> &'static str;

    fn watch_paths(&self, _ctx: &SourceContext) -> Result<Vec<WatchSpec>> {
        Ok(Vec::new())
    }

    fn tick_interval(&self) -> Option<Duration> {
        None
    }

    fn on_tick(&self, _ctx: &SourceContext, _sink: &dyn MutationSink) -> Result<()> {
        Ok(())
    }

    fn on_fs_events(
        &self,
        _ctx: &SourceContext,
        _events: &[SourceEvent],
        _sink: &dyn MutationSink,
    ) -> Result<()> {
        Ok(())
    }

    fn reindex_full(&self, ctx: &SourceContext, sink: &dyn MutationSink) -> Result<()>;

    /// Whether this source should participate in the daemon-driven full
    /// reindex after a schema wipe. Return `false` for sources whose
    /// `reindex_full` is too expensive to run unattended (e.g. multi-minute
    /// full mbox scans). Such sources are expected to be reindexed on
    /// explicit user request (`lupa reindex`) only.
    fn reindex_on_schema_wipe(&self) -> bool {
        true
    }

    fn extra_fields(&self) -> &'static [PluginFieldSpec] {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct CaptureSink(Mutex<Vec<Mutation>>);

    impl MutationSink for CaptureSink {
        fn emit(&self, m: Mutation) -> Result<()> {
            self.0.lock().unwrap().push(m);
            Ok(())
        }
    }

    struct StubSource;

    impl IndexerSource for StubSource {
        fn kind(&self) -> &'static str {
            "stub"
        }
        fn reindex_full(&self, ctx: &SourceContext, sink: &dyn MutationSink) -> Result<()> {
            sink.emit(Mutation::DeleteSourceInstance {
                instance_id: ctx.instance_id.to_string(),
            })?;
            sink.emit(Mutation::Delete {
                doc_id: "stub:1".into(),
            })?;
            Ok(())
        }
    }

    #[test]
    fn indexer_source_reindex_full_emits_expected_mutations() {
        let sink = CaptureSink(Mutex::new(Vec::new()));
        let tmp = std::path::PathBuf::from("/tmp");
        let ctx = SourceContext {
            instance_id: "s1",
            state_dir: &tmp,
        };
        StubSource.reindex_full(&ctx, &sink).unwrap();

        let collected = sink.0.into_inner().unwrap();
        assert_eq!(collected.len(), 2);
        match &collected[0] {
            Mutation::DeleteSourceInstance { instance_id } => assert_eq!(instance_id, "s1"),
            _ => panic!("first mutation must be DeleteSourceInstance"),
        }
        match &collected[1] {
            Mutation::Delete { doc_id } => assert_eq!(doc_id, "stub:1"),
            _ => panic!("second mutation must be Delete"),
        }
    }
}
