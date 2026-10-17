use std::path::{Path, PathBuf};

const ICON: &str = "../../assets/icon/zarn.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const LANG_EN_US: u16 = 0x0409;

fn resource(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes());
    out.extend(32u32.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(flags.to_le_bytes());
    out.extend(LANG_EN_US.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn icon_resources(ico: &[u8]) -> Option<Vec<u8>> {
    if le16(ico, 0)? != 0 || le16(ico, 2)? != 1 {
        return None;
    }
    let count = le16(ico, 4)? as usize;
    let mut out = Vec::new();
    resource(&mut out, 0, 0, 0, &[]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((count as u16).to_le_bytes());
    for n in 0..count {
        let entry = 6 + n * 16;
        let size = le32(ico, entry + 8)? as usize;
        let offset = le32(ico, entry + 12)? as usize;
        let image = ico.get(offset..offset.checked_add(size)?)?;
        let id = n as u16 + 1;
        resource(&mut out, RT_ICON, id, 0x1010, image);
        group.extend(ico.get(entry..entry + 4)?);
        group.extend(1u16.to_le_bytes());
        group.extend(le16(ico, entry + 6)?.max(32).to_le_bytes());
        group.extend((size as u32).to_le_bytes());
        group.extend(id.to_le_bytes());
    }
    resource(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    Some(out)
}

fn embed(icon: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let ico = std::fs::read(icon).map_err(|e| format!("cannot read {}: {e}", icon.display()))?;
    let res = icon_resources(&ico).ok_or_else(|| format!("{} is not a valid .ico file", icon.display()))?;
    let path = out_dir.join("zarn-icon.res");
    std::fs::write(&path, res).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        println!("cargo:warning=zarn.exe is built without its icon: embedding needs the MSVC toolchain");
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    match embed(&manifest.join(ICON), &out_dir) {
        Ok(res) => println!("cargo:rustc-link-arg-bins={}", res.display()),
        Err(err) => println!("cargo:warning=zarn.exe is built without its icon: {err}"),
    }
}
