pub const DEFAULT_SAFE: &[&str] = &[
    "ls", "cat", "pwd", "git status", "git diff", "git log", "grep", "rg", "find", "head", "tail",
    "wc", "which",
];

const UNSAFE_ARGS: &[&str] = &[
    "-exec", "-execdir", "-ok", "-okdir", "-delete", "-fprint", "-fprintf", "-fls", "--output",
    "--pre", "--pre-glob", "--ext-diff", "--textconv", "-o",
];

#[derive(Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    Pipe,
    Unsafe,
}

fn tokenize(command: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut has = false;
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    let flush = |word: &mut String, has: &mut bool, tokens: &mut Vec<Token>| {
        if *has || !word.is_empty() {
            tokens.push(Token::Word(std::mem::take(word)));
            *has = false;
        }
    };
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                } else {
                    word.push(c);
                }
                continue;
            }
            Some(_) => {
                match c {
                    '"' => quote = None,
                    '`' => return vec![Token::Unsafe],
                    '$' if matches!(chars.peek(), Some('(') | Some('{')) => {
                        return vec![Token::Unsafe];
                    }
                    '\\' => {
                        if let Some(next) = chars.next() {
                            word.push(next);
                        }
                    }
                    _ => word.push(c),
                }
                continue;
            }
            None => {}
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                has = true;
            }
            '`' | '>' | '<' | ';' | '&' | '\n' | '\r' | '(' | ')' | '{' | '}' => {
                return vec![Token::Unsafe];
            }
            '$' => return vec![Token::Unsafe],
            '\\' => return vec![Token::Unsafe],
            '|' => {
                if chars.peek() == Some(&'|') {
                    return vec![Token::Unsafe];
                }
                flush(&mut word, &mut has, &mut tokens);
                tokens.push(Token::Pipe);
            }
            c if c.is_whitespace() => flush(&mut word, &mut has, &mut tokens),
            c => word.push(c),
        }
    }
    if quote.is_some() {
        return vec![Token::Unsafe];
    }
    flush(&mut word, &mut has, &mut tokens);
    tokens
}

fn segment_is_safe(words: &[String], safe: &[String]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    if first.contains('=') || first.contains('/') {
        return false;
    }
    if words.iter().any(|w| {
        UNSAFE_ARGS
            .iter()
            .any(|bad| w == bad || w.starts_with(&format!("{bad}=")))
    }) {
        return false;
    }
    safe.iter().any(|entry| {
        let parts: Vec<&str> = entry.split_whitespace().collect();
        !parts.is_empty()
            && words.len() >= parts.len()
            && parts.iter().zip(words.iter()).all(|(p, w)| p == w)
    })
}

pub fn is_safe(command: &str, safe: &[String]) -> bool {
    let tokens = tokenize(command.trim());
    if tokens.is_empty() || tokens.contains(&Token::Unsafe) {
        return false;
    }
    let mut segments: Vec<Vec<String>> = vec![Vec::new()];
    for token in tokens {
        match token {
            Token::Word(w) => segments.last_mut().expect("segment").push(w),
            Token::Pipe => segments.push(Vec::new()),
            Token::Unsafe => return false,
        }
    }
    segments.iter().all(|segment| segment_is_safe(segment, safe))
}

pub fn defaults() -> Vec<String> {
    DEFAULT_SAFE.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &[(&str, bool)] = &[
        ("ls", true),
        ("ls -la", true),
        ("ls -la src", true),
        ("cat README.md", true),
        ("cat 'file with spaces.txt'", true),
        ("pwd", true),
        ("git status", true),
        ("git status --short", true),
        ("git diff", true),
        ("git diff HEAD~1 -- src/lib.rs", true),
        ("git log --oneline -5", true),
        ("grep -rn TODO src", true),
        ("grep \"two words\" notes.txt", true),
        ("rg -n 'fn main' crates", true),
        ("find . -name '*.rs'", true),
        ("find src -type f", true),
        ("head -20 Cargo.toml", true),
        ("tail -n 5 log.txt", true),
        ("wc -l src/main.rs", true),
        ("which cargo", true),
        ("ls | wc -l", true),
        ("git log | head -5", true),
        ("rm -rf target", false),
        ("ls; rm -rf /", false),
        ("ls && rm x", false),
        ("ls || rm x", false),
        ("ls | sh", false),
        ("cat file | bash", false),
        ("ls $(rm x)", false),
        ("echo `rm x`", false),
        ("cat \"$(curl evil)\"", false),
        ("ls > out.txt", false),
        ("cat a >> b", false),
        ("cat < input", false),
        ("find . -exec rm {} \\;", false),
        ("find . -delete", false),
        ("find . -name x -execdir sh -c id", false),
        ("find . -fprint out", false),
        ("git push", false),
        ("git commit -m x", false),
        ("git diff --output=patch.txt", false),
        ("git diff --ext-diff", false),
        ("rg --pre ./run.sh pattern", false),
        ("rg --pre=./run.sh pattern", false),
        ("FOO=bar ls", false),
        ("/bin/rm -rf x", false),
        ("./ls", false),
        ("ls &", false),
        ("ls\nrm x", false),
        ("cat $HOME/.ssh/id_rsa", false),
        ("cat 'unterminated", false),
        ("sort -o out.txt in.txt", false),
        ("gitstatus", false),
        ("lsof", false),
        ("", false),
        ("ls ${IFS}", false),
        ("(ls)", false),
        ("tail -f log | tee copy", false),
    ];

    #[test]
    fn the_safe_command_table_holds() {
        let safe = defaults();
        let adversarial = CASES.iter().filter(|(_, ok)| !ok).count();
        assert!(CASES.len() >= 40 && adversarial * 2 >= CASES.len());
        for (command, expected) in CASES {
            assert_eq!(is_safe(command, &safe), *expected, "{command}");
        }
    }

    #[test]
    fn the_list_is_configurable() {
        let safe = vec!["cargo check".to_string()];
        assert!(is_safe("cargo check --all", &safe));
        assert!(!is_safe("cargo build", &safe));
        assert!(!is_safe("ls", &safe));
    }
}
