use anyhow::Result;
use lupa_sources::source::{Mutation as SrcMutation, MutationSink};
use tokio::runtime::Handle;

use crate::index_service::{IndexMutationTx, Mutation as IndexMutation};

pub struct WriterSink {
    tx: IndexMutationTx,
    runtime: Handle,
}

impl WriterSink {
    pub fn new(tx: IndexMutationTx) -> Self {
        Self {
            tx,
            runtime: Handle::current(),
        }
    }
}

impl MutationSink for WriterSink {
    fn emit(&self, mutation: SrcMutation) -> Result<()> {
        let tx = self.tx.clone();
        self.runtime.block_on(async move {
            match mutation {
                SrcMutation::Upsert(boxed) => {
                    tx.send(IndexMutation::UpsertMany(vec![*boxed])).await?;
                }
                SrcMutation::UpsertMany(docs) => {
                    if !docs.is_empty() {
                        tx.send(IndexMutation::UpsertMany(docs)).await?;
                    }
                }
                SrcMutation::Delete { doc_id } => {
                    tx.send(IndexMutation::DeleteMany(vec![doc_id])).await?;
                }
                SrcMutation::DeleteSourceInstance { instance_id } => {
                    tx.send(IndexMutation::DeleteSourceInstance { instance_id })
                        .await?;
                }
            }
            Ok::<_, anyhow::Error>(())
        })?;
        Ok(())
    }
}
