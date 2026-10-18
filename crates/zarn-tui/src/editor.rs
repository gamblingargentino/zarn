use std::time::{SystemTime, UNIX_EPOCH};

pub fn edit_text(initial: &str, editor: Option<&str>) -> Result<String, String> {
    let Some(editor) = editor.filter(|e| !e.trim().is_empty()) else {
        return Err("no external editor: set $VISUAL or $EDITOR, for example export EDITOR=vim".into());
    };
    let words = zarn_config::auth::split_command(editor);
    let Some((program, args)) = words.split_first() else {
        return Err("the editor command is empty".into());
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("zarn-input-{}-{stamp}.md", std::process::id()));
    std::fs::write(&path, initial).map_err(|e| format!("could not write {}: {e}", path.display()))?;
    let status = std::process::Command::new(program).args(args).arg(&path).status();
    let result = match status {
        Err(err) => Err(format!("could not start {program}: {err}")),
        Ok(status) if !status.success() => Err(format!("{program} exited with {status}; the input is unchanged")),
        Ok(_) => std::fs::read_to_string(&path)
            .map(|text| text.trim_end_matches(['\n', '\r']).to_string())
            .map_err(|e| format!("could not read {}: {e}", path.display())),
    };
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_an_editor_it_says_so() {
        assert!(edit_text("x", None).unwrap_err().contains("$VISUAL or $EDITOR"));
    }

    #[cfg(unix)]
    #[test]
    fn the_editor_edits_a_temp_file_that_is_removed_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("ed.sh");
        let log = dir.path().join("path.txt");
        std::fs::write(
            &script,
            format!("printf '%s' \"$1\" > {}\nprintf 'edited: %s\\n' \"$(cat \"$1\")\" > \"$1\"\n", log.display()),
        )
        .unwrap();
        let out = edit_text("draft", Some(&format!("sh {}", script.display()))).unwrap();
        assert_eq!(out, "edited: draft");
        let used = std::fs::read_to_string(&log).unwrap();
        assert!(!std::path::Path::new(&used).exists(), "the temp file is removed");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_editor_leaves_the_input_alone() {
        assert!(edit_text("draft", Some("false")).unwrap_err().contains("unchanged"));
    }
}
