use std::path::PathBuf;

pub fn app_folder() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let folder = executable.parent()?;
    let helper = executable
        .file_name()
        .is_some_and(|name| name == "media-downloader-firefox.exe");
    if helper && folder.file_name().is_some_and(|name| name == "tools") {
        return Some(folder.parent()?.to_path_buf());
    }
    Some(folder.to_path_buf())
}

pub fn portable_root() -> Option<PathBuf> {
    app_folder().filter(|folder| folder.join("portable.flag").is_file())
}
