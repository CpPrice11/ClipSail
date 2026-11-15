use std::{process::Stdio, time::Duration};

use tokio::{io::AsyncReadExt, process::Command};

pub enum MetadataError {
    Tools,
    Unavailable,
}

pub fn command() -> Result<Command, MetadataError> {
    let exe = crate::paths::app_folder()
        .ok_or(MetadataError::Tools)?
        .join("tools/yt-dlp.exe");
    let mut command = Command::new(exe);
    command
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .args([
            "--ignore-config",
            "--no-plugin-dirs",
            "--no-cache-dir",
            "--no-playlist",
            "--no-js-runtimes",
            "--socket-timeout",
            "10",
            "--retries",
            "1",
            "--extractor-retries",
            "1",
        ]);
    Ok(command)
}

pub async fn metadata(
    mut command: Command,
    url: &str,
    template: &str,
) -> Result<Vec<u8>, MetadataError> {
    const MAX_BYTES: usize = 8 * 1024 * 1024;
    let mut child = command
        .args(["--print", template, "--", url])
        .spawn()
        .map_err(|_| MetadataError::Tools)?;
    let stdout = child.stdout.take().ok_or(MetadataError::Unavailable)?;
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut output = Vec::new();
        stdout
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .await
            .map_err(|_| MetadataError::Unavailable)?;
        if output.len() > MAX_BYTES
            || !child
                .wait()
                .await
                .map_err(|_| MetadataError::Unavailable)?
                .success()
        {
            return Err(MetadataError::Unavailable);
        }
        Ok(output)
    })
    .await
    .map_err(|_| MetadataError::Unavailable)?
}
