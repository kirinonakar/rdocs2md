use std::{env, fs, path::PathBuf};

/// Language id and code page of the version strings (English, Unicode).
const LANGUAGE_ID: u16 = 0x0409;
const CODE_PAGE: u16 = 1200;

fn main() {
    // app.res was compiled with rc.exe from `1 ICON "app.ico"`.
    // The ICO contains 16–256 px images generated from app.png.
    println!("cargo::rerun-if-changed=app.res");
    println!("cargo::rerun-if-changed=Cargo.toml");
    println!("cargo::rerun-if-env-changed=CARGO_PKG_VERSION");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    // The Explorer "Details" tab reads a VERSIONINFO resource, so it is
    // generated here from the Cargo.toml metadata instead of being checked in.
    let version = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("version.res");
    fs::write(&version, version_resource()).expect("failed to write version.res");
    for resource in [manifest.join("app.res"), version] {
        println!("cargo::rustc-link-arg-bin=rdocs2md={}", resource.display());
    }
}

/// Builds the VS_VERSION_INFO resource data shown by the Explorer "Details" tab.
fn version_resource() -> Vec<u8> {
    let name = env::var("CARGO_PKG_NAME").unwrap();
    let description = env::var("CARGO_PKG_DESCRIPTION").unwrap();
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = version_numbers(&version);

    let mut strings = Vec::new();
    let table = begin_block(&mut strings, &format!("{LANGUAGE_ID:04X}{CODE_PAGE:04X}"), 0, 1);
    for (key, value) in [
        ("FileDescription", description.as_str()),
        ("FileVersion", version.as_str()),
        ("InternalName", name.as_str()),
        ("LegalCopyright", "Copyright (c) rdocs2md contributors"),
        ("OriginalFilename", "rdocs2md.exe"),
        ("ProductName", name.as_str()),
        ("ProductVersion", version.as_str()),
    ] {
        let value = utf16(value);
        let start = begin_block(&mut strings, key, (value.len() / 2) as u16, 1);
        strings.extend_from_slice(&value);
        end_block(&mut strings, start);
    }
    end_block(&mut strings, table);

    let mut translation = Vec::new();
    let start = begin_block(&mut translation, "Translation", 4, 0);
    translation.extend_from_slice(&LANGUAGE_ID.to_le_bytes());
    translation.extend_from_slice(&CODE_PAGE.to_le_bytes());
    end_block(&mut translation, start);

    let [major, minor, patch, revision] = numbers;
    let file_version_ms = (u32::from(major) << 16) | u32::from(minor);
    let file_version_ls = (u32::from(patch) << 16) | u32::from(revision);

    // The fixed part is VS_FIXEDFILEINFO.
    let mut data = Vec::new();
    let root = begin_block(&mut data, "VS_VERSION_INFO", 52, 0);
    for word in [
        0xFEEF_04BD,     // VS_FFI_SIGNATURE
        0x0001_0000,     // VS_FFI_STRUCVERSION
        file_version_ms, // FileVersion
        file_version_ls,
        file_version_ms, // ProductVersion
        file_version_ls,
        0,               // FileFlagsMask
        0,               // FileFlags
        0x0004_0004,     // VOS_NT_WINDOWS32
        1,               // VFT_APP
        0,               // FileSubtype
        0,               // FileDateMS
        0,               // FileDateLS
    ] {
        data.extend_from_slice(&word.to_le_bytes());
    }
    pad_to_dword(&mut data);

    let start = begin_block(&mut data, "StringFileInfo", 0, 1);
    data.extend_from_slice(&strings);
    end_block(&mut data, start);

    let start = begin_block(&mut data, "VarFileInfo", 0, 1);
    data.extend_from_slice(&translation);
    end_block(&mut data, start);

    end_block(&mut data, root);
    pad_to_dword(&mut data);

    resource_file(&data)
}

/// Splits `1.2.3` into the four numeric fields of VS_FIXEDFILEINFO.
fn version_numbers(version: &str) -> [u16; 4] {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut numbers = [0u16; 4];
    for (slot, part) in numbers.iter_mut().zip(core.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    numbers
}

/// Wraps version data in the .res layout that link.exe consumes.
fn resource_file(data: &[u8]) -> Vec<u8> {
    // rc.exe starts the file with a null entry that link.exe skips.
    let mut res = Vec::new();
    res.extend_from_slice(&0u32.to_le_bytes()); // DataSize
    res.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    res.extend_from_slice(&[0xFF, 0xFF]);
    res.extend_from_slice(&0u16.to_le_bytes()); // type: ordinal 0
    res.extend_from_slice(&[0xFF, 0xFF]);
    res.extend_from_slice(&0u16.to_le_bytes()); // name: ordinal 0
    res.extend_from_slice(&[0; 16]);
    res.extend_from_slice(&(data.len() as u32).to_le_bytes());
    res.extend_from_slice(&32u32.to_le_bytes()); // header: two size fields, type, name, fixed fields
    res.extend_from_slice(&[0xFF, 0xFF]); // resource type: ordinal follows
    res.extend_from_slice(&16u16.to_le_bytes()); // RT_VERSION
    res.extend_from_slice(&[0xFF, 0xFF]); // resource name: ordinal follows
    res.extend_from_slice(&1u16.to_le_bytes()); // 1 (VS_VERSION_INFO)
    res.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    res.extend_from_slice(&0x0030u16.to_le_bytes()); // MemoryFlags
    res.extend_from_slice(&LANGUAGE_ID.to_le_bytes());
    res.extend_from_slice(&0u32.to_le_bytes()); // Version
    res.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    res.extend_from_slice(data);
    res
}

/// Starts a version block aligned to a DWORD and returns its offset.
fn begin_block(out: &mut Vec<u8>, key: &str, value_length: u16, value_type: u16) -> usize {
    pad_to_dword(out);
    let start = out.len();
    out.extend_from_slice(&[0, 0]); // wLength, filled in by end_block
    out.extend_from_slice(&value_length.to_le_bytes());
    out.extend_from_slice(&value_type.to_le_bytes());
    out.extend_from_slice(&utf16(key));
    pad_to_dword(out);
    start
}

fn end_block(out: &mut Vec<u8>, start: usize) {
    let length = (out.len() - start) as u16;
    out[start..start + 2].copy_from_slice(&length.to_le_bytes());
}

fn utf16(value: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(value.len() * 2 + 2);
    for unit in value.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes
}

fn pad_to_dword(out: &mut Vec<u8>) {
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
