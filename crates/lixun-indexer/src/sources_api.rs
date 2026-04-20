use anyhow::Result;

pub trait IndexerSources: Send + Sync {
    fn build_fs_source(&self) -> Result<lixun_sources::fs::FsSource>;
    fn exclude(&self) -> &[String];
    fn max_file_size_mb(&self) -> u64;
}
