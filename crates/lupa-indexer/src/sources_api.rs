use anyhow::Result;

pub trait IndexerSources: Send + Sync {
    fn build_fs_source(&self) -> Result<lupa_sources::fs::FsSource>;
    fn build_sources(&self) -> Result<Vec<Box<dyn lupa_sources::Source>>>;
    fn exclude(&self) -> &[String];
    fn max_file_size_mb(&self) -> u64;
}
