use std::fs;
use std::io;
use std::path::Path;

pub fn write_atomic(path: &Path, contents: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "zarn".to_string());
    let temp = parent.join(format!(".{name}.zarn-tmp"));

    if let Err(err) = fs::write(&temp, contents) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }

    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zarn-fsx-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn it_writes_a_new_file_and_creates_parents() {
        let dir = temp_dir("new");
        let path = dir.join("nested/deep/file.txt");
        write_atomic(&path, "hello").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn it_replaces_existing_content_and_leaves_no_temp_file() {
        let dir = temp_dir("replace");
        let path = dir.join("file.txt");
        write_atomic(&path, "first").unwrap();
        write_atomic(&path, "second").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("zarn-tmp"))
            .collect();
        assert!(leftovers.is_empty());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_failed_write_leaves_the_original_intact() {
        let dir = temp_dir("fail");
        let path = dir.join("file.txt");
        write_atomic(&path, "original").unwrap();

        let blocked = dir.join("blocked");
        fs::create_dir_all(&blocked).unwrap();
        assert!(write_atomic(&blocked, "nope").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "original");
        fs::remove_dir_all(dir).ok();
    }
}
