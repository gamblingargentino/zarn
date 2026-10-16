use serde_json::Value;
use std::path::Path;

pub const CONTEXT: usize = 3;
const MAX_CELLS: usize = 4_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffLine {
    Hunk(String),
    Context { old: usize, new: usize, text: String },
    Added { new: usize, text: String },
    Removed { old: usize, text: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub tool: String,
    pub path: String,
    pub new_file: bool,
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
}

impl Preview {
    pub fn summary(&self) -> String {
        format!("+{} -{}", self.added, self.removed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Equal(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn ops(old: &[&str], new: &[&str]) -> Vec<Op> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    let a = &old[prefix..old.len() - suffix];
    let b = &new[prefix..new.len() - suffix];
    let mut out: Vec<Op> = (0..prefix).map(|i| Op::Equal(i, i)).collect();
    if a.len().saturating_mul(b.len()) > MAX_CELLS {
        out.extend((0..a.len()).map(|i| Op::Delete(prefix + i)));
        out.extend((0..b.len()).map(|j| Op::Insert(prefix + j)));
    } else {
        let (n, m) = (a.len(), b.len());
        let mut table = vec![0u32; (n + 1) * (m + 1)];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                table[i * (m + 1) + j] = if a[i] == b[j] {
                    table[(i + 1) * (m + 1) + j + 1] + 1
                } else {
                    table[(i + 1) * (m + 1) + j].max(table[i * (m + 1) + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && a[i] == b[j] {
                out.push(Op::Equal(prefix + i, prefix + j));
                i += 1;
                j += 1;
            } else if i < n && (j == m || table[(i + 1) * (m + 1) + j] >= table[i * (m + 1) + j + 1]) {
                out.push(Op::Delete(prefix + i));
                i += 1;
            } else {
                out.push(Op::Insert(prefix + j));
                j += 1;
            }
        }
    }
    let (old_base, new_base) = (old.len() - suffix, new.len() - suffix);
    out.extend((0..suffix).map(|k| Op::Equal(old_base + k, new_base + k)));
    out
}

pub fn unified(old: &str, new: &str, context: usize) -> Vec<DiffLine> {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let ops = ops(&a, &b);
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Equal(..)))
        .map(|(i, _)| i)
        .collect();
    let mut out = Vec::new();
    let mut k = 0;
    while k < changed.len() {
        let start = changed[k].saturating_sub(context);
        let mut end = changed[k];
        while k < changed.len() && changed[k] <= end + 2 * context + 1 {
            end = changed[k];
            k += 1;
        }
        let end = (end + context + 1).min(ops.len());
        let (old_start, new_start) = ops[start..end]
            .iter()
            .fold((None, None), |(o, n), op| match op {
                Op::Equal(x, y) => (o.or(Some(*x)), n.or(Some(*y))),
                Op::Delete(x) => (o.or(Some(*x)), n),
                Op::Insert(y) => (o, n.or(Some(*y))),
            });
        let old_count = ops[start..end].iter().filter(|op| !matches!(op, Op::Insert(_))).count();
        let new_count = ops[start..end].iter().filter(|op| !matches!(op, Op::Delete(_))).count();
        out.push(DiffLine::Hunk(format!(
            "@@ -{},{old_count} +{},{new_count} @@",
            old_start.map(|x| x + 1).unwrap_or(0),
            new_start.map(|y| y + 1).unwrap_or(0)
        )));
        for op in &ops[start..end] {
            out.push(match *op {
                Op::Equal(x, y) => DiffLine::Context {
                    old: x + 1,
                    new: y + 1,
                    text: a[x].to_string(),
                },
                Op::Delete(x) => DiffLine::Removed {
                    old: x + 1,
                    text: a[x].to_string(),
                },
                Op::Insert(y) => DiffLine::Added {
                    new: y + 1,
                    text: b[y].to_string(),
                },
            });
        }
    }
    out
}

fn apply_edit(content: &str, edit: &Value) -> Option<String> {
    let old = edit.get("old_string")?.as_str()?;
    let new = edit.get("new_string")?.as_str()?;
    if old.is_empty() || !content.contains(old) {
        return None;
    }
    let all = edit.get("replace_all").and_then(Value::as_bool).unwrap_or(false);
    Some(if all {
        content.replace(old, new)
    } else {
        content.replacen(old, new, 1)
    })
}

pub fn preview(tool: &str, args: &Value, cwd: &Path) -> Option<Preview> {
    let path = args.get("path").and_then(Value::as_str)?;
    let full = if Path::new(path).is_absolute() {
        Path::new(path).to_path_buf()
    } else {
        cwd.join(path)
    };
    let existing = std::fs::read_to_string(&full).ok();
    let (old, new) = match tool {
        "write" => {
            let content = args.get("content").and_then(Value::as_str)?;
            (existing.clone().unwrap_or_default(), content.to_string())
        }
        "edit" => {
            let content = existing.clone()?;
            let updated = apply_edit(&content, args)?;
            (content, updated)
        }
        "multiedit" => {
            let content = existing.clone()?;
            let mut updated = content.clone();
            for edit in args.get("edits")?.as_array()? {
                updated = apply_edit(&updated, edit)?;
            }
            (content, updated)
        }
        _ => return None,
    };
    let new_file = existing.is_none();
    let lines = if new_file {
        new.lines()
            .enumerate()
            .map(|(i, l)| DiffLine::Added {
                new: i + 1,
                text: l.to_string(),
            })
            .collect()
    } else {
        unified(&old, &new, CONTEXT)
    };
    let added = lines.iter().filter(|l| matches!(l, DiffLine::Added { .. })).count();
    let removed = lines.iter().filter(|l| matches!(l, DiffLine::Removed { .. })).count();
    Some(Preview {
        tool: tool.to_string(),
        path: path.to_string(),
        new_file,
        lines,
        added,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_one_line_change_has_context_and_line_numbers() {
        let old = (1..=10).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let new = old.replace("line 5", "line five");
        let diff = unified(&old, &new, 3);
        assert_eq!(diff[0], DiffLine::Hunk("@@ -2,7 +2,7 @@".into()));
        assert!(diff.contains(&DiffLine::Removed { old: 5, text: "line 5".into() }));
        assert!(diff.contains(&DiffLine::Added { new: 5, text: "line five".into() }));
        assert_eq!(diff.len(), 1 + 3 + 2 + 3);
    }

    #[test]
    fn distant_changes_make_separate_hunks() {
        let old = (1..=40).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        let new = old.replace("l3\n", "x3\n").replace("l35", "x35");
        let hunks = unified(&old, &new, 3)
            .iter()
            .filter(|l| matches!(l, DiffLine::Hunk(_)))
            .count();
        assert_eq!(hunks, 2);
    }

    #[test]
    fn previews_never_write_and_mark_new_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        let edit = preview(
            "edit",
            &json!({ "path": "a.txt", "old_string": "two", "new_string": "2" }),
            dir.path(),
        )
        .unwrap();
        assert_eq!(edit.summary(), "+1 -1");
        assert_eq!(std::fs::read_to_string(dir.path().join("a.txt")).unwrap(), "one\ntwo\n");
        let new = preview("write", &json!({ "path": "b.txt", "content": "x\ny" }), dir.path()).unwrap();
        assert!(new.new_file);
        assert_eq!(new.added, 2);
        assert!(!dir.path().join("b.txt").exists());
        assert!(preview("edit", &json!({ "path": "a.txt", "old_string": "zzz", "new_string": "" }), dir.path()).is_none());
    }
}
