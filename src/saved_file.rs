use std::{
    fs::{self, Metadata},
    path::{Component, Path, PathBuf},
};

pub fn resolve(folder: &Path, name: &str) -> Option<(PathBuf, Metadata)> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return None;
    }
    let path = folder.join(name);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let folder = folder.canonicalize().ok()?;
    let resolved = path.canonicalize().ok()?;
    (resolved.parent() == Some(folder.as_path())).then_some((path, metadata))
}
