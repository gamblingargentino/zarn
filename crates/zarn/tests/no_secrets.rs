use regex::Regex;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Patterns {
    rules: Vec<(String, Regex)>,
    allow: Vec<String>,
}

fn root() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let text = path.display().to_string();
    match text.strip_prefix(r"\\?\") {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

fn patterns() -> Patterns {
    let text = std::fs::read_to_string(root().join("scripts/secret-patterns.txt")).unwrap();
    let mut rules = Vec::new();
    let mut allow = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (name, body) = line.split_once('\t').expect("name<TAB>pattern");
        if name == "allow" {
            allow.push(body.to_string());
        } else {
            rules.push((name.to_string(), Regex::new(body).expect("valid pattern")));
        }
    }
    assert!(!rules.is_empty());
    Patterns { rules, allow }
}

impl Patterns {
    fn hits(&self, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            for (name, rule) in &self.rules {
                let flagged = rule
                    .find_iter(line)
                    .any(|m| !self.allow.iter().any(|a| m.as_str().contains(a.as_str())));
                if flagged {
                    out.push(format!("{name} pattern on line {}", n + 1));
                }
            }
        }
        out
    }
}

fn repo_files() -> Vec<PathBuf> {
    let output = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .current_dir(root())
        .output()
        .expect("git is needed to list the repository files");
    assert!(output.status.success());
    output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| root().join(String::from_utf8_lossy(p).as_ref()))
        .filter(|p| p.is_file())
        .collect()
}

fn printable_runs(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut run = String::new();
    for byte in bytes {
        if (0x20..0x7f).contains(byte) {
            run.push(*byte as char);
        } else {
            if run.len() >= 8 {
                out.push_str(&run);
                out.push('\n');
            }
            run.clear();
        }
    }
    if run.len() >= 8 {
        out.push_str(&run);
    }
    out
}

fn fake(prefix: &str, body: &str, len: usize) -> String {
    format!("{prefix}{}", body.repeat(len / body.len() + 1)[..len].to_string())
}

#[test]
fn the_patterns_catch_every_key_shape_they_name() {
    let p = patterns();
    let samples = [
        fake("sk-ant-api03-", "Ab3dE6", 40),
        fake("sk-proj-", "Zx9Qw2", 40),
        fake("AIza", "Sy7Kq1", 35),
        fake("gsk_", "Lm4Np8", 40),
        format!("OPENAI_API_KEY={}", fake("", "QUJDREVGR0g", 44)),
        format!("export GITHUB_TOKEN=\"{}\"", fake("", "ghp4Xy7", 36)),
    ];
    for sample in &samples {
        assert!(!p.hits(sample).is_empty(), "a key shape went undetected");
    }
    for clean in [
        "a risk-based-permission-defaults-policy",
        "api_key_env = \"OPENAI_API_KEY\"",
        "let key = \"test-key-not-real-0123456789abcdef0123\";",
        "SECRET_KEY = \"sk-test-key-not-real-abcdefghijklmnop\"",
    ] {
        assert!(p.hits(clean).is_empty(), "false positive on {clean:?}");
    }
}

#[test]
fn no_file_in_the_repository_contains_a_key() {
    let p = patterns();
    let mut findings = Vec::new();
    for path in repo_files() {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        if bytes.contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for hit in p.hits(&text) {
            let rel = path.strip_prefix(root()).unwrap_or(&path).display().to_string();
            findings.push(format!("{rel}: {hit}"));
        }
    }
    assert!(findings.is_empty(), "possible keys (values withheld):\n{}", findings.join("\n"));
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn remap_flags() -> (String, Vec<String>) {
    let root = root();
    let home = home();
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".cargo")));
    let mut flags = Vec::new();
    let mut private = Vec::new();
    if let Some(home) = &home {
        flags.push(format!("--remap-path-prefix={}=~", home.display()));
        private.push(home.display().to_string());
    }
    if let Some(cargo_home) = &cargo_home {
        flags.push(format!("--remap-path-prefix={}=/cargo", cargo_home.display()));
        private.push(cargo_home.display().to_string());
    }
    flags.push(format!("--remap-path-prefix={}=/zarn", root.display()));
    private.push(root.display().to_string());
    (flags.join("\u{1f}"), private)
}

#[test]
fn the_release_binary_contains_no_key_and_no_machine_path() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let target = root().join("target").join("dist");
    let (flags, private) = remap_flags();
    let status = Command::new(cargo)
        .args(["build", "--release", "-p", "zarn", "--bin", "zarn"])
        .current_dir(root())
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_ENCODED_RUSTFLAGS", &flags)
        .env_remove("RUSTFLAGS")
        .status()
        .expect("cargo build --release");
    assert!(status.success(), "the release build failed");

    let name = if cfg!(windows) { "zarn.exe" } else { "zarn" };
    let binary = target.join("release").join(name);
    let bytes = std::fs::read(&binary).expect("release binary");
    assert!(bytes.len() > 1_000_000, "suspiciously small binary");

    let strings = printable_runs(&bytes);
    let findings = patterns().hits(&strings);
    assert!(
        findings.is_empty(),
        "possible keys in {} (values withheld):\n{}",
        binary.display(),
        findings.join("\n")
    );

    let lowered = strings.to_lowercase();
    for path in private {
        let variants = [path.clone(), path.replace('\\', "/"), path.replace('\\', "\\\\")];
        for variant in variants {
            if variant.len() > 3 {
                assert!(
                    !lowered.contains(&variant.to_lowercase()),
                    "the release binary embeds a path from the build machine ({variant})"
                );
            }
        }
    }
}
