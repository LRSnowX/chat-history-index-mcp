use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Adopt,
    Copy,
}

impl ImportMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Adopt => "adopt",
            Self::Copy => "copy",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagedPaths {
    pub root: PathBuf,
    pub sources_dir: PathBuf,
    pub db_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub tmp_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub archive_path: PathBuf,
    pub db_path: PathBuf,
}

impl ManagedPaths {
    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(&self.sources_dir)?;
        fs::create_dir_all(&self.db_dir)?;
        fs::create_dir_all(&self.logs_dir)?;
        fs::create_dir_all(&self.tmp_dir)?;
        fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct DataHome {
    root: PathBuf,
}

impl DataHome {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn default_root() -> PathBuf {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("Library")
            .join("Application Support")
            .join("chat-history-index-mcp")
    }

    pub fn from_option(root: Option<PathBuf>) -> Self {
        Self::new(root.unwrap_or_else(Self::default_root))
    }

    pub fn paths(&self) -> ManagedPaths {
        let sources_dir = self.root.join("sources");
        let db_dir = self.root.join("db");
        let logs_dir = self.root.join("logs");
        let tmp_dir = self.root.join("tmp");
        let cache_dir = self.root.join("cache");
        ManagedPaths {
            root: self.root.clone(),
            archive_path: sources_dir.join("openai-export.zip"),
            db_path: db_dir.join("index.sqlite3"),
            sources_dir,
            db_dir,
            logs_dir,
            tmp_dir,
            cache_dir,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn stage_archive(
        &self,
        source_archive: &Path,
        mode: ImportMode,
    ) -> anyhow::Result<(PathBuf, ImportMode)> {
        let paths = self.paths();
        paths.ensure()?;

        if paths.archive_path == source_archive {
            return Ok((paths.archive_path, ImportMode::Adopt));
        }

        if paths.archive_path.exists() {
            let source_meta = fs::metadata(source_archive)?;
            let target_meta = fs::metadata(&paths.archive_path)?;
            if source_meta.len() == target_meta.len() {
                return Ok((paths.archive_path, ImportMode::Copy));
            }
            fs::remove_file(&paths.archive_path)?;
        }

        match mode {
            ImportMode::Adopt => match fs::rename(source_archive, &paths.archive_path) {
                Ok(()) => Ok((paths.archive_path, ImportMode::Adopt)),
                Err(error) if cross_device(error.kind()) => {
                    fs::copy(source_archive, &paths.archive_path)?;
                    Ok((paths.archive_path, ImportMode::Copy))
                }
                Err(error) => Err(error.into()),
            },
            ImportMode::Copy => {
                fs::copy(source_archive, &paths.archive_path)?;
                Ok((paths.archive_path, ImportMode::Copy))
            }
        }
    }
}

fn cross_device(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::CrossesDevices | io::ErrorKind::PermissionDenied
    )
}
