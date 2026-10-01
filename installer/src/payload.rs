// ------------ Installer Payload ------------
// The launcher files travel inside setup.exe: tar + zstd glued to the end, then a manifest and a footer with a
// SHA-256. `pack` builds that during the build, and install checks and unpacks it into the install folder.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const FOOTER_MAGIC: &[u8; 8] = b"PBFYINST";
pub const FORMAT_VERSION: u32 = 1;
pub const FOOTER_LEN: usize = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footer {
    pub format_version: u32,
    pub payload_offset: u64,
    pub payload_len: u64,
    pub manifest_offset: u64,
    pub manifest_len: u64,
    pub sha256: [u8; 32],
}

impl Footer {
    pub fn to_bytes(self) -> [u8; FOOTER_LEN] {
        let mut out = [0u8; FOOTER_LEN];
        out[0..8].copy_from_slice(FOOTER_MAGIC);
        out[8..12].copy_from_slice(&self.format_version.to_le_bytes());
        out[16..24].copy_from_slice(&self.payload_offset.to_le_bytes());
        out[24..32].copy_from_slice(&self.payload_len.to_le_bytes());
        out[32..40].copy_from_slice(&self.manifest_offset.to_le_bytes());
        out[40..48].copy_from_slice(&self.manifest_len.to_le_bytes());
        out[48..80].copy_from_slice(&self.sha256);
        out
    }

    pub fn from_bytes(buf: &[u8; FOOTER_LEN]) -> Option<Self> {
        if &buf[0..8] != FOOTER_MAGIC {
            return None;
        }
        let u32le = |r: std::ops::Range<usize>| u32::from_le_bytes(buf[r].try_into().unwrap());
        let u64le = |r: std::ops::Range<usize>| u64::from_le_bytes(buf[r].try_into().unwrap());
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&buf[48..80]);
        Some(Self {
            format_version: u32le(8..12),
            payload_offset: u64le(16..24),
            payload_len: u64le(24..32),
            manifest_offset: u64le(32..40),
            manifest_len: u64le(40..48),
            sha256,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadManifest {
    pub format_version: u32,
    pub version: String,
    pub main_binary: String,
    pub estimated_size_kb: u64,
    pub file_count: u64,
    pub created_at: String,
}

#[derive(Clone)]
pub struct Payload {
    pub exe_path: PathBuf,
    pub footer: Footer,
    pub manifest: PayloadManifest,
}

impl Payload {
    pub fn open(exe: &Path) -> Result<Option<Payload>, String> {
        let mut f = File::open(exe).map_err(|e| format!("open {}: {e}", exe.display()))?;
        let total = f.seek(SeekFrom::End(0)).map_err(|e| format!("seek: {e}"))?;
        if total < FOOTER_LEN as u64 {
            return Ok(None);
        }
        f.seek(SeekFrom::End(-(FOOTER_LEN as i64)))
            .map_err(|e| format!("seek: {e}"))?;
        let mut buf = [0u8; FOOTER_LEN];
        f.read_exact(&mut buf)
            .map_err(|e| format!("read footer: {e}"))?;
        let Some(footer) = Footer::from_bytes(&buf) else {
            return Ok(None);
        };
        if footer.format_version != FORMAT_VERSION {
            return Err(format!(
                "payload format v{} is not supported by this stub (expected v{FORMAT_VERSION})",
                footer.format_version
            ));
        }
        let manifest_end = footer
            .manifest_offset
            .checked_add(footer.manifest_len)
            .ok_or("corrupt footer: manifest range overflows")?;
        let payload_end = footer
            .payload_offset
            .checked_add(footer.payload_len)
            .ok_or("corrupt footer: payload range overflows")?;
        if footer.manifest_offset != payload_end
            || manifest_end != total - FOOTER_LEN as u64
        {
            return Err("corrupt footer: section offsets are inconsistent".into());
        }

        f.seek(SeekFrom::Start(footer.manifest_offset))
            .map_err(|e| format!("seek manifest: {e}"))?;
        let mut manifest_bytes = vec![0u8; footer.manifest_len as usize];
        f.read_exact(&mut manifest_bytes)
            .map_err(|e| format!("read manifest: {e}"))?;
        let manifest: PayloadManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| format!("parse payload manifest: {e}"))?;
        if !is_plain_file_name(&manifest.main_binary) {
            return Err(format!(
                "payload manifest names {:?} as the launcher, which is not a plain file name",
                manifest.main_binary
            ));
        }

        Ok(Some(Payload {
            exe_path: exe.to_path_buf(),
            footer,
            manifest,
        }))
    }

    pub fn open_current_exe() -> Result<Option<Payload>, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
        Self::open(&exe)
    }

    pub fn verify(&self) -> Result<(), String> {
        self.verify_with_progress(|_| {})
    }

    pub fn verify_with_progress(&self, mut progress: impl FnMut(f32)) -> Result<(), String> {
        let mut f = File::open(&self.exe_path).map_err(|e| format!("open: {e}"))?;
        f.seek(SeekFrom::Start(self.footer.payload_offset))
            .map_err(|e| format!("seek: {e}"))?;
        let total = (self.footer.payload_len + self.footer.manifest_len).max(1);
        let mut remaining = self.footer.payload_len + self.footer.manifest_len;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            f.read_exact(&mut buf[..want])
                .map_err(|e| format!("read payload: {e}"))?;
            hasher.update(&buf[..want]);
            remaining -= want as u64;
            progress((total - remaining) as f32 / total as f32);
        }
        if hasher.finalize().as_slice() != self.footer.sha256 {
            return Err("payload checksum mismatch: the installer file is corrupt".into());
        }
        Ok(())
    }

    pub fn extract_to(
        &self,
        dest: &Path,
        cancel: &crate::msg::Cancel,
        mut progress: impl FnMut(f32),
    ) -> Result<Vec<String>, crate::msg::EngineError> {
        let f = File::open(&self.exe_path).map_err(|e| format!("open: {e}"))?;
        let region = SectionReader::new(f, self.footer.payload_offset, self.footer.payload_len)
            .map_err(|e| format!("seek payload: {e}"))?;
        let payload_len = self.footer.payload_len.max(1);
        let counted = CountingReader::new(region);
        let counter = counted.counter();
        let zstd =
            zstd::stream::read::Decoder::new(counted).map_err(|e| format!("zstd init: {e}"))?;
        let mut archive = tar::Archive::new(zstd);

        std::fs::create_dir_all(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
        let mut written = Vec::new();
        for entry in archive.entries().map_err(|e| format!("tar entries: {e}"))? {
            cancel.check()?;
            let mut entry = entry.map_err(|e| format!("tar entry: {e}"))?;
            let rel = entry
                .path()
                .map_err(|e| format!("tar path: {e}"))?
                .to_string_lossy()
                .replace('\\', "/");
            let kind = entry.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                return Err(format!("refused to extract {rel}: it is not a plain file or folder").into());
            }
            if !entry
                .unpack_in(dest)
                .map_err(|e| format!("extract {rel}: {e}"))?
            {
                return Err(format!("refused to extract unsafe path: {rel}").into());
            }
            if entry.header().entry_type().is_file() {
                written.push(rel);
            }
            progress(
                (counter.load(std::sync::atomic::Ordering::Relaxed) as f32 / payload_len as f32)
                    .min(1.0),
            );
        }
        progress(1.0);
        Ok(written)
    }

    pub fn write_stub_copy(&self, dest: &Path) -> Result<(), String> {
        let mut src = File::open(&self.exe_path).map_err(|e| format!("open: {e}"))?;
        let mut out = File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
        let mut remaining = self.footer.payload_offset;
        let mut buf = vec![0u8; 1 << 20];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            src.read_exact(&mut buf[..want])
                .map_err(|e| format!("read stub: {e}"))?;
            out.write_all(&buf[..want])
                .map_err(|e| format!("write stub: {e}"))?;
            remaining -= want as u64;
        }
        Ok(())
    }
}

pub fn pack(
    stub: &Path,
    payload_dir: &Path,
    out: &Path,
    version: &str,
    main_binary: &str,
) -> Result<PayloadManifest, String> {
    let mut files = Vec::new();
    collect_files(payload_dir, payload_dir, &mut files)?;
    if files.is_empty() {
        return Err(format!("payload dir {} is empty", payload_dir.display()));
    }
    if !files.iter().any(|(rel, _)| rel == main_binary) {
        return Err(format!(
            "main binary '{main_binary}' not found in payload root"
        ));
    }

    let stub_bytes =
        std::fs::read(stub).map_err(|e| format!("read stub {}: {e}", stub.display()))?;
    if Footer::from_bytes_at_end(&stub_bytes).is_some() {
        return Err("stub already has a payload appended".into());
    }

    let total_bytes: u64 =
        files.iter().map(|(_, size)| size).sum::<u64>() + stub_bytes.len() as u64;
    let manifest = PayloadManifest {
        format_version: FORMAT_VERSION,
        version: version.into(),
        main_binary: main_binary.into(),
        estimated_size_kb: total_bytes / 1024,
        file_count: files.len() as u64,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let manifest_bytes =
        serde_json::to_vec(&manifest).map_err(|e| format!("serialize manifest: {e}"))?;

    let mut out_file = File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?;
    out_file
        .write_all(&stub_bytes)
        .map_err(|e| format!("write stub: {e}"))?;
    let payload_offset = stub_bytes.len() as u64;

    {
        let mut encoder = zstd::stream::write::Encoder::new(&mut out_file, 19)
            .map_err(|e| format!("zstd init: {e}"))?;
        let mut tar = tar::Builder::new(&mut encoder);
        for (rel, _) in &files {
            let full = payload_dir.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            let mut f = File::open(&full).map_err(|e| format!("open {}: {e}", full.display()))?;
            tar.append_file(rel, &mut f)
                .map_err(|e| format!("tar {}: {e}", rel))?;
        }
        tar.finish().map_err(|e| format!("tar finish: {e}"))?;
        drop(tar);
        encoder.finish().map_err(|e| format!("zstd finish: {e}"))?;
    }

    let manifest_offset = out_file
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("seek: {e}"))?;
    let payload_len = manifest_offset - payload_offset;
    out_file
        .write_all(&manifest_bytes)
        .map_err(|e| format!("write manifest: {e}"))?;

    let mut hasher = Sha256::new();
    {
        let mut f = File::open(out).map_err(|e| format!("reopen out: {e}"))?;
        f.seek(SeekFrom::Start(payload_offset))
            .map_err(|e| format!("seek: {e}"))?;
        let mut remaining = payload_len + manifest_bytes.len() as u64;
        let mut buf = vec![0u8; 1 << 20];
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            f.read_exact(&mut buf[..want])
                .map_err(|e| format!("hash pass: {e}"))?;
            hasher.update(&buf[..want]);
            remaining -= want as u64;
        }
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hasher.finalize());

    let footer = Footer {
        format_version: FORMAT_VERSION,
        payload_offset,
        payload_len,
        manifest_offset,
        manifest_len: manifest_bytes.len() as u64,
        sha256,
    };
    out_file
        .write_all(&footer.to_bytes())
        .map_err(|e| format!("write footer: {e}"))?;
    out_file.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(manifest)
}

fn is_plain_file_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
        && !name.contains([':', '/', '\\'])
}

impl Footer {
    fn from_bytes_at_end(bytes: &[u8]) -> Option<Footer> {
        if bytes.len() < FOOTER_LEN {
            return None;
        }
        let tail: &[u8; FOOTER_LEN] = bytes[bytes.len() - FOOTER_LEN..].try_into().ok()?;
        Footer::from_bytes(tail)
    }
}

fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, u64)>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("read_dir {}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("read_dir entry: {e}"))?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let meta = entry.metadata().map_err(|e| format!("stat: {e}"))?;
        if meta.is_dir() {
            collect_files(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| format!("strip_prefix: {e}"))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, meta.len()));
        }
    }
    Ok(())
}

struct SectionReader {
    file: File,
    remaining: u64,
}

impl SectionReader {
    fn new(mut file: File, offset: u64, len: u64) -> std::io::Result<Self> {
        file.seek(SeekFrom::Start(offset))?;
        Ok(Self {
            file,
            remaining: len,
        })
    }
}

impl Read for SectionReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let want = buf.len().min(self.remaining as usize);
        let n = self.file.read(&mut buf[..want])?;
        self.remaining -= n as u64;
        Ok(n)
    }
}

struct CountingReader<R> {
    inner: R,
    count: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl<R> CountingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            count: Default::default(),
        }
    }
    fn counter(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        self.count.clone()
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count
            .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_with_the_retired_name_fields_still_parses() {
        let json = br#"{"formatVersion":1,"productName":"Peebify Launcher","publisher":"Peebify","version":"1.2.3","mainBinary":"peebify-launcher.exe","estimatedSizeKb":10,"fileCount":2,"createdAt":"2026-01-01T00:00:00Z"}"#;
        let manifest: PayloadManifest = serde_json::from_slice(json).unwrap();
        assert_eq!(manifest.version, "1.2.3");
        assert_eq!(manifest.main_binary, "peebify-launcher.exe");
    }

    #[test]
    fn main_binary_must_be_a_plain_file_name() {
        assert!(is_plain_file_name("Peebify Launcher.exe"));
        for bad in ["", ".", "..", r"..\x.exe", "a/b.exe", r"C:\x.exe", "C:x.exe", "x.exe:ads", r"\\s\x.exe"] {
            assert!(!is_plain_file_name(bad), "{bad}");
        }
    }
}
