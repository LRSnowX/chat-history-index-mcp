use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::Context;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use zip::ZipArchive;

#[derive(Debug, Clone)]
pub struct ManagedArchive {
    pub path: PathBuf,
    pub sha256_hex: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ConversationZipMaterialization {
    pub outer_member: String,
    pub temp_path: PathBuf,
}

pub fn digest_file(path: &Path) -> anyhow::Result<ManagedArchive> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 64];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let sha256_hex = hex::encode(hasher.finalize());
    let size_bytes = path.metadata()?.len();
    Ok(ManagedArchive {
        path: path.to_path_buf(),
        sha256_hex,
        size_bytes,
    })
}

pub fn list_outer_members(path: &Path) -> anyhow::Result<Vec<String>> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut members = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        members.push(entry.name().to_string());
    }
    Ok(members)
}

pub fn materialize_nested_member(
    archive_path: &Path,
    outer_member: &str,
    temp_dir: &Path,
) -> anyhow::Result<ConversationZipMaterialization> {
    let file = File::open(archive_path)?;
    let mut outer = ZipArchive::new(file)?;
    let mut member = outer
        .by_name(outer_member)
        .with_context(|| format!("opening nested member {outer_member}"))?;
    let mut temp = NamedTempFile::new_in(temp_dir)?;
    std::io::copy(&mut member, &mut temp)?;
    let temp_path = temp.into_temp_path().keep()?;
    Ok(ConversationZipMaterialization {
        outer_member: outer_member.to_string(),
        temp_path,
    })
}

pub fn read_json_member<T: serde::de::DeserializeOwned>(
    nested_archive_path: &Path,
    member_name: &str,
) -> anyhow::Result<T> {
    let file = File::open(nested_archive_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut member = archive.by_name(member_name)?;
    let mut buffer = String::new();
    member.read_to_string(&mut buffer)?;
    Ok(serde_json::from_str(&buffer)?)
}

pub fn list_nested_members(nested_archive_path: &Path) -> anyhow::Result<Vec<String>> {
    let file = File::open(nested_archive_path)?;
    let mut archive = ZipArchive::new(file)?;
    let mut members = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        members.push(entry.name().to_string());
    }
    Ok(members)
}

pub fn build_asset_index(paths: &[String]) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for path in paths {
        for token in extract_file_tokens(path) {
            index.entry(token).or_default().push(path.clone());
        }
    }
    index
}

pub fn extract_file_tokens(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut tokens = BTreeSet::new();
    let mut index = 0;
    while index + 5 < bytes.len() {
        if &bytes[index..index + 5] == b"file_" {
            let start = index;
            index += 5;
            while index < bytes.len() {
                let ch = bytes[index] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                    index += 1;
                } else {
                    break;
                }
            }
            tokens.insert(text[start..index].to_string());
        } else {
            index += 1;
        }
    }
    tokens
}
