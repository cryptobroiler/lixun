use lupa_core::PluginFieldSpec;
use lupa_sources::IndexerSource;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub struct SourceInstance {
    pub instance_id: String,
    pub state_dir: PathBuf,
    pub source: Arc<dyn IndexerSource>,
}

pub struct SourceRegistry {
    pub instances: Vec<SourceInstance>,
    pub plugin_fields_by_kind: BTreeMap<&'static str, &'static [PluginFieldSpec]>,
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self {
            instances: Vec::new(),
            plugin_fields_by_kind: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        instance_id: String,
        state_dir_root: &std::path::Path,
        source: Arc<dyn IndexerSource>,
    ) {
        let kind = source.kind();
        let fields = source.extra_fields();
        if !fields.is_empty() {
            self.plugin_fields_by_kind.entry(kind).or_insert(fields);
        }
        let state_dir = state_dir_root.join(kind).join(&instance_id);
        let _ = std::fs::create_dir_all(&state_dir);
        self.instances.push(SourceInstance {
            instance_id,
            state_dir,
            source,
        });
    }
}

impl Default for SourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
