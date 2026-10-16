use std::path::PathBuf;

pub const CONFIG_DIR_ENV: &str = "ZARN_CONFIG_DIR";
pub const PROJECT_DIR: &str = ".zarn";

pub fn user_dir() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(CONFIG_DIR_ENV)
        && !explicit.is_empty()
    {
        return Some(PathBuf::from(explicit));
    }
    dirs::home_dir().map(|home| home.join(".config").join("zarn"))
}

pub fn user_config() -> Option<PathBuf> {
    user_dir().map(|dir| dir.join("config.toml"))
}

pub fn auth_file() -> Option<PathBuf> {
    user_dir().map(|dir| dir.join("auth.json"))
}

pub fn project_dir() -> PathBuf {
    PathBuf::from(PROJECT_DIR)
}

pub fn project_config() -> PathBuf {
    project_dir().join("config.toml")
}

pub fn local_config() -> PathBuf {
    project_dir().join(crate::layers::LOCAL_FILE)
}

pub fn data_dir() -> PathBuf {
    let sessions = zarn_core::session::data_root();
    sessions
        .parent()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(sessions)
}
