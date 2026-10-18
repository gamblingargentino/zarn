use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const REPO: &str = "gamblingargentino/zarn";
pub const API: &str = "https://api.github.com";
pub const REPO_ENV: &str = "ZARN_UPDATE_REPO";
pub const API_ENV: &str = "ZARN_UPDATE_API";
pub const SKIP_ENV: &str = "ZARN_NO_UPDATE_CHECK";
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
const SUMS: &str = "SHA256SUMS";
const TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    #[serde(default)]
    pub digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub html_url: String,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub checked_at: u64,
    pub latest: Option<String>,
}

pub fn repo() -> String {
    std::env::var(REPO_ENV).ok().filter(|r| !r.is_empty()).unwrap_or_else(|| REPO.to_string())
}

fn api() -> String {
    std::env::var(API_ENV).ok().filter(|r| !r.is_empty()).unwrap_or_else(|| API.to_string())
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn target() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{arch}-pc-windows-msvc"),
        "macos" => format!("{arch}-apple-darwin"),
        "linux" => format!("{arch}-unknown-linux-gnu"),
        other => format!("{arch}-{other}"),
    }
}

pub async fn update_self() -> Result<Outcome> {
    let exe = std::env::current_exe().context("finding the running zarn binary")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    update(VERSION, &target(), &exe).await
}

pub fn asset_name(target: &str) -> String {
    if target.contains("windows") {
        format!("zarn-{target}.exe")
    } else {
        format!("zarn-{target}")
    }
}

fn parts(version: &str) -> (Vec<u64>, bool) {
    let version = version.trim().trim_start_matches('v');
    let (core, pre) = match version.split_once('-') {
        Some((core, _)) => (core, true),
        None => (version, false),
    };
    (core.split('.').map(|p| p.parse().unwrap_or(0)).collect(), pre)
}

pub fn newer(candidate: &str, current: &str) -> bool {
    let (a, a_pre) = parts(candidate);
    let (b, b_pre) = parts(current);
    let len = a.len().max(b.len());
    for i in 0..len {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    b_pre && !a_pre
}

fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("zarn/{}", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()?)
}

fn token() -> Option<String> {
    ["ZARN_GITHUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
}

pub async fn latest() -> Result<Release> {
    let url = format!("{}/repos/{}/releases/latest", api(), repo());
    let mut request = client(TIMEOUT)?.get(&url).header("accept", "application/vnd.github+json");
    if let Some(token) = token() {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.context("asking GitHub for the latest release")?;
    match response.status().as_u16() {
        200 => Ok(response.json().await.context("reading the release from GitHub")?),
        404 => bail!("{} has no published release yet", repo()),
        403 | 429 => bail!("GitHub is rate limiting update checks right now; try again later or set GITHUB_TOKEN"),
        code => bail!("GitHub answered HTTP {code} for {url}"),
    }
}

async fn download(url: &str) -> Result<Vec<u8>> {
    let mut request = client(DOWNLOAD_TIMEOUT)?.get(url);
    if let Some(token) = token().filter(|_| url.starts_with(&api())) {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.with_context(|| format!("downloading {url}"))?;
    if !response.status().is_success() {
        bail!("downloading {url} failed: HTTP {}", response.status().as_u16());
    }
    Ok(response.bytes().await?.to_vec())
}

async fn expected_hash(release: &Release, asset: &Asset) -> Result<String> {
    if let Some(digest) = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
        return Ok(digest.to_lowercase());
    }
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == SUMS)
        .with_context(|| format!("release {} has no checksum for {}, so it is not installed", release.tag_name, asset.name))?;
    let text = String::from_utf8(download(&sums.browser_download_url).await?).context("SHA256SUMS is not text")?;
    text.lines()
        .find_map(|line| {
            let (hash, name) = line.split_once(char::is_whitespace)?;
            (name.trim().trim_start_matches('*') == asset.name).then(|| hash.trim().to_lowercase())
        })
        .with_context(|| format!("SHA256SUMS has no line for {}", asset.name))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate(String),
    Installed { from: String, to: String, path: PathBuf },
}

impl Outcome {
    pub fn summary(&self) -> String {
        match self {
            Outcome::UpToDate(version) => format!("zarn {version} is the latest version"),
            Outcome::Installed { from, to, path } => format!(
                "updated zarn {from} → {to} at {}; restart zarn to use it",
                path.display()
            ),
        }
    }
}

pub async fn update(current: &str, target: &str, exe: &Path) -> Result<Outcome> {
    let release = latest().await?;
    if !newer(release.version(), current) {
        return Ok(Outcome::UpToDate(current.to_string()));
    }
    let name = asset_name(target);
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == name)
        .with_context(|| format!("release {} has no build for this platform ({name}); download it from {}", release.tag_name, release.html_url))?;
    let want = expected_hash(&release, asset).await?;
    let bytes = download(&asset.browser_download_url).await?;
    let got = sha256_hex(&bytes);
    if got != want {
        bail!("the download of {name} does not match its published checksum, so it was not installed");
    }
    replace(exe, &bytes).with_context(|| {
        format!(
            "could not replace {}; if zarn is installed for all users, run the new installer from {} instead",
            exe.display(),
            release.html_url
        )
    })?;
    Ok(Outcome::Installed {
        from: current.to_string(),
        to: release.version().to_string(),
        path: exe.to_path_buf(),
    })
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(suffix);
    exe.with_file_name(name)
}

pub fn replace(exe: &Path, bytes: &[u8]) -> Result<()> {
    let fresh = sibling(exe, ".new");
    std::fs::write(&fresh, bytes).with_context(|| format!("writing {}", fresh.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fresh, std::fs::Permissions::from_mode(0o755))?;
    }
    let old = sibling(exe, ".old");
    let _ = std::fs::remove_file(&old);
    if cfg!(windows) {
        if let Err(err) = std::fs::rename(exe, &old) {
            let _ = std::fs::remove_file(&fresh);
            return Err(err).with_context(|| format!("moving {} aside", exe.display()));
        }
        if let Err(err) = std::fs::rename(&fresh, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(err).with_context(|| format!("putting the new {} in place", exe.display()));
        }
    } else if let Err(err) = std::fs::rename(&fresh, exe) {
        let _ = std::fs::remove_file(&fresh);
        return Err(err).with_context(|| format!("putting the new {} in place", exe.display()));
    }
    Ok(())
}

pub fn cleanup(exe: &Path) {
    let _ = std::fs::remove_file(sibling(exe, ".old"));
    let _ = std::fs::remove_file(sibling(exe, ".new"));
}

fn seen_path() -> PathBuf {
    crate::paths::data_dir().join("update.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn load_seen(path: &Path) -> Seen {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub async fn notice(current: &str) -> Option<String> {
    if std::env::var_os(SKIP_ENV).is_some() {
        return None;
    }
    notice_at(current, &seen_path(), CHECK_EVERY).await
}

pub async fn notice_at(current: &str, path: &Path, every: Duration) -> Option<String> {
    let mut seen = load_seen(path);
    if now().saturating_sub(seen.checked_at) >= every.as_secs() {
        seen = Seen {
            checked_at: now(),
            latest: latest().await.ok().map(|r| r.version().to_string()).or(seen.latest),
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string(&seen).unwrap_or_default());
    }
    let latest = seen.latest?;
    newer(&latest, current).then(|| format!("zarn {latest} is available (you have {current}): run /update, or zarn update"))
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

pub fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend(((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        assert_eq!(
            sha256_hex(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn versions_compare_by_number_and_releases_beat_prereleases() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("v0.10.0", "0.9.3"));
        assert!(newer("1.0.0", "1.0.0-rc.1"));
        assert!(!newer("1.0.0-rc.1", "1.0.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn asset_names_follow_the_target() {
        assert_eq!(asset_name("x86_64-pc-windows-msvc"), "zarn-x86_64-pc-windows-msvc.exe");
        assert_eq!(asset_name("aarch64-apple-darwin"), "zarn-aarch64-apple-darwin");
    }

    #[test]
    fn replacing_swaps_the_file_and_cleanup_removes_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("zarn");
        std::fs::write(&exe, b"old").unwrap();
        replace(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        std::fs::write(sibling(&exe, ".old"), b"x").unwrap();
        cleanup(&exe);
        assert!(!sibling(&exe, ".old").exists());
        assert!(!sibling(&exe, ".new").exists());
    }
}
