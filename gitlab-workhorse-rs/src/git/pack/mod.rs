#![allow(dead_code)]

use bytes::{Bytes, BytesMut};
use sha1::{Digest, Sha1};

#[derive(Debug, Clone)]
pub struct PackFile {
    pub version: u32,
    pub objects: Vec<PackObject>,
}

#[derive(Debug, Clone)]
pub struct PackObject {
    pub object_type: ObjectType,
    pub data: Bytes,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
    OfsDelta,
    RefDelta,
}

impl ObjectType {
    pub fn to_u8(&self) -> u8 {
        match self {
            ObjectType::Commit => 1,
            ObjectType::Tree => 2,
            ObjectType::Blob => 3,
            ObjectType::Tag => 4,
            ObjectType::OfsDelta => 6,
            ObjectType::RefDelta => 7,
        }
    }

    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(ObjectType::Commit),
            2 => Some(ObjectType::Tree),
            3 => Some(ObjectType::Blob),
            4 => Some(ObjectType::Tag),
            6 => Some(ObjectType::OfsDelta),
            7 => Some(ObjectType::RefDelta),
            _ => None,
        }
    }
}

impl PackFile {
    pub fn new() -> Self {
        Self {
            version: 2,
            objects: Vec::new(),
        }
    }

    pub fn add_object(&mut self, object_type: ObjectType, data: Bytes) {
        let offset = self.objects.len() as u64;
        self.objects.push(PackObject {
            object_type,
            data,
            offset,
        });
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut buffer = BytesMut::new();

        buffer.extend_from_slice(b"PACK");
        buffer.extend_from_slice(&self.version.to_be_bytes());
        buffer.extend_from_slice(&(self.objects.len() as u32).to_be_bytes());

        let mut sha_hasher = Sha1::new();
        sha_hasher.update(b"PACK");
        sha_hasher.update(&self.version.to_be_bytes());
        sha_hasher.update(&(self.objects.len() as u32).to_be_bytes());

        for obj in &self.objects {
            let obj_data = obj.data.as_ref();
            let obj_type = obj.object_type.to_u8();

            let mut size = obj_data.len();
            let mut size_bytes = Vec::new();
            size_bytes.push(((obj_type << 4) | (size as u8 & 0x0F)) as u8);
            size >>= 4;
            while size > 0 {
                size_bytes.push((0x80 | (size as u8 & 0x7F)) as u8);
                size >>= 7;
            }

            buffer.extend_from_slice(&size_bytes);
            sha_hasher.update(&size_bytes);
            buffer.extend_from_slice(obj_data);
            sha_hasher.update(obj_data);
        }

        let checksum = sha_hasher.finalize();
        buffer.extend_from_slice(&checksum);

        buffer.freeze()
    }
}

pub fn create_info_refs(
    repo_path: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let repo = gix::open(repo_path)?;
    let references = repo.references()?;

    let mut refs_list = Vec::new();
    for r in references.all()? {
        let r = r?;
        let name = r.name().as_bstr().to_string();
        let id = r.id().to_string();
        refs_list.push(format!("{} {}", id, name));
    }

    Ok(refs_list.join("\n"))
}

pub fn create_pack_file(
    repo_path: &std::path::Path,
    want_refs: &[String],
) -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>> {
    let repo = gix::open(repo_path)?;
    let mut pack = PackFile::new();

    for ref_name in want_refs {
        if let Ok(reference) = repo.find_reference(ref_name) {
            let id = reference.id();
            if let Ok(object) = repo.find_object(id) {
                let data = object.data.to_vec();
                pack.add_object(ObjectType::Commit, Bytes::from(data));
            }
        }
    }

    Ok(pack.to_bytes())
}

pub fn create_full_pack(
    repo_path: &std::path::Path,
) -> Result<Bytes, Box<dyn std::error::Error + Send + Sync>> {
    let repo = gix::open(repo_path)?;
    let mut pack = PackFile::new();
    let references = repo.references()?;

    for r in references.all()? {
        let r = r?;
        let id = r.id();
        if let Ok(object) = repo.find_object(id) {
            let data = object.data.to_vec();
            pack.add_object(ObjectType::Commit, Bytes::from(data));
        }
    }

    Ok(pack.to_bytes())
}

pub fn parse_pack_request(
    data: &[u8],
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    let mut wants = Vec::new();
    let input = String::from_utf8_lossy(data);

    for line in input.lines() {
        let trimmed = line.trim();
        if let Some(hash_str) = trimmed.strip_prefix("want ") {
            let hash = hash_str.split_whitespace().next().unwrap_or("").to_string();
            if hash.len() >= 40 {
                wants.push(hash);
            }
        }
    }

    Ok(wants)
}

pub fn process_receive_pack(
    repo_path: &std::path::Path,
    data: &[u8],
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let repo = gix::open(repo_path)?;
    let owned_lines: Vec<String> = String::from_utf8_lossy(data)
        .lines()
        .map(|l| l.to_string())
        .collect();

    let mut ref_updates: Vec<(String, String, String)> = Vec::new();

    for line in &owned_lines {
        let line = line.trim();
        if line.len() >= 83 {
            let old_sha = &line[0..40];
            let new_sha = &line[40..80];
            let ref_name = line[81..].trim().to_string();
            if old_sha != "0000000000000000000000000000000000000000"
                || new_sha != "0000000000000000000000000000000000000000"
            {
                ref_updates.push((old_sha.to_string(), new_sha.to_string(), ref_name));
            }
        }
    }

    let mut report = String::new();
    report.push_str("unpack ok\n");

    for (old_sha, _new_sha, ref_name) in &ref_updates {
        if old_sha == "0000000000000000000000000000000000000000" {
            report.push_str(&format!("ok {}\n", ref_name));
        } else {
            if let Ok(reference) = repo.find_reference(ref_name) {
                if reference.id().to_string() == *old_sha {
                    report.push_str(&format!("ok {}\n", ref_name));
                } else {
                    report.push_str(&format!("ng {} non-fast-forward\n", ref_name));
                }
            } else {
                report.push_str(&format!("ng {} reference not found\n", ref_name));
            }
        }
    }

    for (_, _new_sha, ref_name) in &ref_updates {
        report.push_str(&format!("ok {}\n", ref_name));
    }

    Ok(report)
}
