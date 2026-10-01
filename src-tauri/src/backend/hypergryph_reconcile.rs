// ------------ Endfield Package Installer ------------
// Endfield ships as split zip packs. Instead of saving them whole, this reads each pack's file index over HTTP range
// requests and extracts the game files straight into the install folder. It keeps a manifest of what should be there
// so it can verify files, repair only the broken ones, and update by fetching just the files that changed.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::fs_util::{manifest_key, safe_join};

pub const MANIFEST_FILE: &str = "PeebifyManifest.json";
const MANIFEST_FORMAT: &str = "hypergryph-split-zip-v1";

const REMOTE_PROBE_CHUNK: u64 = 256 * 1024;
const RANGE_RETRIES: u32 = 3;
const RANGE_RETRY_DELAY_MS: u64 = 1500;
const IO_BUF: usize = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackRef {
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub size: u64,
    pub crc32: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: String,
    pub packs: Vec<PackRef>,
    pub files: Vec<ManifestEntry>,
}

pub fn load_manifest(install_dir: &Path) -> Option<Manifest> {
    let path = install_dir.join(MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        Err(e) => {
            log::warn!("Could not read {}: {e}", path.display());
            return None;
        }
    };
    let manifest: Manifest = match serde_json::from_str(&text) {
        Ok(manifest) => manifest,
        Err(e) => {
            log::warn!("Ignoring unreadable {}: {e}", path.display());
            return None;
        }
    };
    if manifest.format != MANIFEST_FORMAT {
        log::warn!(
            "Ignoring {} with unknown format {:?}",
            path.display(),
            manifest.format
        );
        return None;
    }
    Some(manifest)
}

fn manifest_from_entries(packs: Vec<PackRef>, version: &str, entries: Vec<CdEntry>) -> Manifest {
    Manifest {
        format: MANIFEST_FORMAT.to_string(),
        version: version.to_string(),
        packs,
        files: entries
            .into_iter()
            .map(|e| ManifestEntry {
                path: e.path,
                size: e.size,
                crc32: e.crc32,
            })
            .collect(),
    }
}

fn remote_entries(
    packs: &[PackRef],
    cancelled: &Arc<AtomicBool>,
    handle: &tokio::runtime::Handle,
    hooks: Option<&dyn Hooks>,
) -> Result<Vec<CdEntry>, String> {
    let mut reader =
        HttpRangeReader::open(packs.to_vec(), Arc::clone(cancelled), handle.clone(), hooks)?;
    read_central_directory(&mut reader)
}

pub fn manifest_from_remote(
    packs: Vec<PackRef>,
    version: &str,
    cancelled: Arc<AtomicBool>,
    handle: tokio::runtime::Handle,
) -> Result<Manifest, String> {
    let entries = remote_entries(&packs, &cancelled, &handle, None)?;
    Ok(manifest_from_entries(packs, version, entries))
}

pub fn remote_totals(
    packs: Vec<PackRef>,
    cancelled: Arc<AtomicBool>,
    handle: tokio::runtime::Handle,
) -> Result<(u64, u64), String> {
    let entries = remote_entries(&packs, &cancelled, &handle, None)?;
    Ok(entry_totals(&entries))
}

fn entry_totals(entries: &[CdEntry]) -> (u64, u64) {
    (
        entries.iter().map(|e| e.size).sum(),
        entries.len() as u64,
    )
}

pub fn manifest_from_local_zip(
    zip_path: &Path,
    url: &str,
    version: &str,
) -> Result<Manifest, String> {
    let size = std::fs::metadata(zip_path)
        .map_err(|e| format!("could not stat {}: {e}", zip_path.display()))?
        .len();
    let mut file = std::fs::File::open(zip_path)
        .map_err(|e| format!("could not open {}: {e}", zip_path.display()))?;
    let entries = read_central_directory(&mut file)?;
    Ok(manifest_from_entries(
        vec![PackRef {
            url: url.to_string(),
            size,
        }],
        version,
        entries,
    ))
}

pub fn save_manifest(install_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let text = serde_json::to_string(manifest).map_err(|e| e.to_string())?;
    super::fs_util::write_atomic(&install_dir.join(MANIFEST_FILE), text.as_bytes())
}

// ------------ Progress Events ------------
// What the installer reports back while it works, so the caller can show progress and honor pause or cancel.
pub enum Event<'a> {
    Phase {
        message: String,
        name: &'a str,
    },
    Totals {
        total_bytes: u64,
        sizes: Vec<(String, u64)>,
    },
    Bytes {
        path: &'a str,
        delta: u64,
        done: bool,
    },
}

pub trait Hooks: super::progress::Control {
    fn event(&self, event: Event);
    fn status(&self, _message: &str) {}
    fn checking(&self, _event: Event) {}
}

// ------------ Reading Packs ------------
// Two readers that make the split packs look like one big file: one for parts already on disk and one that
// streams the same bytes over HTTP range requests, with retries and waiting out network outages.
struct LocalSplitReader {
    files: Vec<(std::fs::File, u64)>,
    starts: Vec<u64>,
    total: u64,
    pos: u64,
}

impl LocalSplitReader {
    fn open(parts: &[PathBuf]) -> io::Result<Self> {
        let mut files = Vec::with_capacity(parts.len());
        let mut starts = Vec::with_capacity(parts.len());
        let mut total = 0u64;
        for part in parts {
            let file = std::fs::File::open(part)?;
            let size = file.metadata()?.len();
            starts.push(total);
            files.push((file, size));
            total += size;
        }
        Ok(Self {
            files,
            starts,
            total,
            pos: 0,
        })
    }

    fn locate(&self, pos: u64) -> usize {
        match self.starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }
}

impl Read for LocalSplitReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total || out.is_empty() {
            return Ok(0);
        }
        let idx = self.locate(self.pos);
        let (file, size) = &mut self.files[idx];
        let local_off = self.pos - self.starts[idx];
        let available = (*size - local_off).min(out.len() as u64) as usize;
        file.seek(SeekFrom::Start(local_off))?;
        let n = file.read(&mut out[..available])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for LocalSplitReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = resolve_seek(self.pos, self.total, from)?;
        Ok(self.pos)
    }
}

struct HttpRangeReader<'h> {
    client: &'static reqwest::Client,
    handle: tokio::runtime::Handle,
    parts: Vec<PackRef>,
    starts: Vec<u64>,
    total: u64,
    pos: u64,
    buf: bytes::Bytes,
    buf_start: u64,
    stream: Option<RangeStream>,
    limit: Option<u64>,
    cancelled: Arc<AtomicBool>,
    hooks: Option<&'h dyn Hooks>,
}

const STREAM_BATCH: usize = 1024 * 1024;
const STREAM_AHEAD: usize = 8;
const CANCEL_POLL: Duration = Duration::from_millis(250);

type StreamItem = Result<bytes::Bytes, (String, bool)>;

struct RangeStream {
    start: u64,
    next: u64,
    end: u64,
    rx: tokio::sync::mpsc::Receiver<StreamItem>,
    pump: tokio::task::JoinHandle<()>,
}

impl Drop for RangeStream {
    fn drop(&mut self) {
        self.pump.abort();
    }
}

enum RangeFailure {
    Fatal(io::Error),
    Gone(String),
    Retry { message: String, transport: bool },
}

trait BoundedRead: Read + Seek {
    fn bound(&mut self, _end: Option<u64>) {}
}

impl BoundedRead for HttpRangeReader<'_> {
    fn bound(&mut self, end: Option<u64>) {
        self.limit = end;
    }
}

fn range_end(pos: u64, chunk: u64, total: u64, limit: Option<u64>) -> u64 {
    let end = (pos + chunk).min(total);
    match limit {
        Some(limit) => end.min(limit).max(pos + 1).min(total),
        None => end,
    }
}

impl<'h> HttpRangeReader<'h> {
    fn open(
        parts: Vec<PackRef>,
        cancelled: Arc<AtomicBool>,
        handle: tokio::runtime::Handle,
        hooks: Option<&'h dyn Hooks>,
    ) -> Result<Self, String> {
        if parts.is_empty() {
            return Err("no pack URLs to read from".to_string());
        }
        let client = super::http::download_client();
        let mut starts = Vec::with_capacity(parts.len());
        let mut total = 0u64;
        for part in &parts {
            starts.push(total);
            total += part.size;
        }
        Ok(Self {
            client,
            handle,
            parts,
            starts,
            total,
            pos: 0,
            buf: bytes::Bytes::new(),
            buf_start: 0,
            stream: None,
            limit: None,
            cancelled,
            hooks,
        })
    }

    fn locate(&self, pos: u64) -> usize {
        match self.starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    fn with_retries<T>(
        &self,
        mut attempt_once: impl FnMut() -> Result<T, RangeFailure>,
    ) -> io::Result<T> {
        let mut last_err = String::new();
        let mut attempt = 1u32;
        while attempt <= RANGE_RETRIES {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(io::Error::other("cancelled"));
            }
            match attempt_once() {
                Ok(value) => return Ok(value),
                Err(RangeFailure::Fatal(e)) => return Err(e),
                Err(RangeFailure::Gone(message)) => {
                    last_err = message;
                    break;
                }
                Err(RangeFailure::Retry { message, transport }) => {
                    last_err = message;
                    if transport {
                        if let Some(hooks) = self.hooks {
                            if wait_for_network(&self.handle, &self.cancelled, hooks) {
                                continue;
                            }
                        }
                    }
                }
            }
            if attempt < RANGE_RETRIES {
                std::thread::sleep(Duration::from_millis(
                    RANGE_RETRY_DELAY_MS * u64::from(attempt),
                ));
            }
            attempt += 1;
        }
        Err(io::Error::other(last_err))
    }

    fn send_range(
        &self,
        part: usize,
        offset: u64,
        len: u64,
    ) -> Result<reqwest::Response, RangeFailure> {
        let url = &self.parts[part].url;
        let range = format!("bytes={}-{}", offset, offset + len - 1);
        let result = self.handle.block_on(async {
            self.client
                .get(url)
                .header(reqwest::header::RANGE, &range)
                .send()
                .await
        });
        match result {
            Ok(resp) => match resp.status().as_u16() {
                206 => Ok(resp),
                200 => Err(RangeFailure::Fatal(io::Error::other(
                    "CDN ignored the Range request (returned 200)",
                ))),
                status if is_gone_status(status) => {
                    Err(RangeFailure::Gone(format!("HTTP {status} for {url}")))
                }
                status => Err(RangeFailure::Retry {
                    message: format!("HTTP {status} for {url}"),
                    transport: false,
                }),
            },
            Err(e) => Err(RangeFailure::Retry {
                message: format!("range request failed: {e}"),
                transport: true,
            }),
        }
    }

    fn fetch_range(&self, part: usize, offset: u64, len: u64) -> io::Result<bytes::Bytes> {
        self.with_retries(|| {
            let resp = self.send_range(part, offset, len)?;
            match self.handle.block_on(resp.bytes()) {
                Ok(bytes) if bytes.len() as u64 == len => Ok(bytes),
                Ok(bytes) => Err(RangeFailure::Retry {
                    message: format!("short range response ({} of {len} bytes)", bytes.len()),
                    transport: false,
                }),
                Err(e) => Err(RangeFailure::Retry {
                    message: format!("range body error: {e}"),
                    transport: true,
                }),
            }
        })
    }

    fn open_stream(&self, part: usize, start: u64, end: u64) -> Result<RangeStream, RangeFailure> {
        let len = end - start;
        let mut resp = self.send_range(part, start - self.starts[part], len)?;
        let (tx, rx) = tokio::sync::mpsc::channel::<StreamItem>(STREAM_AHEAD);
        let pump = self.handle.spawn(async move {
            let mut batch = bytes::BytesMut::new();
            let mut received = 0u64;
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        received += chunk.len() as u64;
                        if received > len {
                            let message = format!("short range response ({received} of {len} bytes)");
                            let _ = tx.send(Err((message, false))).await;
                            return;
                        }
                        let ready = if batch.is_empty() && chunk.len() >= STREAM_BATCH {
                            chunk
                        } else {
                            batch.extend_from_slice(&chunk);
                            if batch.len() < STREAM_BATCH {
                                continue;
                            }
                            batch.split().freeze()
                        };
                        if tx.send(Ok(ready)).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {
                        if !batch.is_empty() {
                            let _ = tx.send(Ok(batch.split().freeze())).await;
                        }
                        return;
                    }
                    Err(e) => {
                        if !batch.is_empty() && tx.send(Ok(batch.split().freeze())).await.is_err() {
                            return;
                        }
                        let _ = tx.send(Err((format!("range body error: {e}"), true))).await;
                        return;
                    }
                }
            }
        });
        Ok(RangeStream {
            start,
            next: start,
            end,
            rx,
            pump,
        })
    }

    fn next_batch(&self, stream: &mut RangeStream) -> Result<bytes::Bytes, RangeFailure> {
        let cancelled = &self.cancelled;
        let rx = &mut stream.rx;
        let item = self.handle.block_on(async {
            loop {
                match tokio::time::timeout(CANCEL_POLL, rx.recv()).await {
                    Ok(item) => return Some(item),
                    Err(_) if cancelled.load(Ordering::SeqCst) => return None,
                    Err(_) => {}
                }
            }
        });
        match item {
            None => Err(RangeFailure::Fatal(io::Error::other("cancelled"))),
            Some(Some(Ok(bytes))) => Ok(bytes),
            Some(Some(Err((message, transport)))) => Err(RangeFailure::Retry { message, transport }),
            Some(None) => Err(RangeFailure::Retry {
                message: format!(
                    "short range response ({} of {} bytes)",
                    stream.next - stream.start,
                    stream.end - stream.start
                ),
                transport: false,
            }),
        }
    }

    fn stream_fill(&mut self, pos: u64) -> io::Result<()> {
        let idx = self.locate(pos);
        let part_end = self.starts[idx] + self.parts[idx].size;
        let end = range_end(pos, self.total - pos, self.total, self.limit).min(part_end);
        let mut stream = self
            .stream
            .take()
            .filter(|s| s.next == pos && s.next < s.end);
        let (mut live, bytes) = self.with_retries(|| {
            let mut live = match stream.take() {
                Some(live) => live,
                None => self.open_stream(idx, pos, end)?,
            };
            let bytes = self.next_batch(&mut live)?;
            Ok((live, bytes))
        })?;
        live.next += bytes.len() as u64;
        if live.next < live.end {
            self.stream = Some(live);
        }
        self.buf = bytes;
        self.buf_start = pos;
        Ok(())
    }

    fn fill(&mut self, pos: u64) -> io::Result<()> {
        let sequential = !self.buf.is_empty() && pos == self.buf_start + self.buf.len() as u64;
        if sequential {
            return self.stream_fill(pos);
        }
        self.stream = None;
        let end = range_end(pos, REMOTE_PROBE_CHUNK, self.total, self.limit);
        let mut pieces: Vec<bytes::Bytes> = Vec::with_capacity(1);
        let mut cursor = pos;
        while cursor < end {
            let idx = self.locate(cursor);
            let part_end = self.starts[idx] + self.parts[idx].size;
            let want = end.min(part_end) - cursor;
            let local_off = cursor - self.starts[idx];
            let bytes = self.fetch_range(idx, local_off, want)?;
            cursor += bytes.len() as u64;
            pieces.push(bytes);
        }
        self.buf = match pieces.len() {
            1 => pieces.pop().unwrap_or_default(),
            _ => bytes::Bytes::from(pieces.concat()),
        };
        self.buf_start = pos;
        Ok(())
    }
}

impl Read for HttpRangeReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.total || out.is_empty() {
            return Ok(0);
        }
        let in_buffer =
            self.pos >= self.buf_start && self.pos < self.buf_start + self.buf.len() as u64;
        if !in_buffer {
            self.fill(self.pos)?;
        }
        let offset = (self.pos - self.buf_start) as usize;
        let n = out.len().min(self.buf.len() - offset);
        out[..n].copy_from_slice(&self.buf[offset..offset + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for HttpRangeReader<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        self.pos = resolve_seek(self.pos, self.total, from)?;
        Ok(self.pos)
    }
}

fn is_gone_status(status: u16) -> bool {
    matches!(status, 403 | 404 | 410)
}

const OFFLINE_POLL: Duration = Duration::from_secs(3);
static OUTAGE: super::http::OutageGate = super::http::OutageGate::new();

fn wait_for_network(
    handle: &tokio::runtime::Handle,
    cancelled: &AtomicBool,
    hooks: &dyn Hooks,
) -> bool {
    use super::http;

    if cancelled.load(Ordering::SeqCst) || handle.block_on(http::is_online()) {
        return false;
    }
    handle.block_on(OUTAGE.single_flight(|| async {
        log::warn!("reconcile: connection lost, waiting for it to come back.");
        http::note_unreachable();
        hooks.status(super::sophon::STATUS_OFFLINE);
        let mut waited = Duration::ZERO;
        while !cancelled.load(Ordering::SeqCst) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            waited += Duration::from_millis(250);
            if waited >= OFFLINE_POLL {
                waited = Duration::ZERO;
                if http::is_online().await {
                    break;
                }
            }
        }
        if cancelled.load(Ordering::SeqCst) {
            return;
        }
        log::info!("reconcile: connection restored, resuming.");
        http::note_reachable();
        hooks.status("");
    }));
    !cancelled.load(Ordering::SeqCst)
}

fn resolve_seek(pos: u64, total: u64, from: SeekFrom) -> io::Result<u64> {
    let target: i128 = match from {
        SeekFrom::Start(offset) => offset as i128,
        SeekFrom::End(offset) => total as i128 + offset as i128,
        SeekFrom::Current(offset) => pos as i128 + offset as i128,
    };
    if target < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "seek before start",
        ));
    }
    Ok(target as u64)
}

// ------------ Zip Index Parsing ------------
// Reads the zip central directory (including zip64) to list every file in the packs, with its size and checksum.
#[derive(Debug, Clone)]
struct CdEntry {
    path: String,
    size: u64,
    csize: u64,
    crc32: u32,
    method: u16,
    flags: u16,
    header_start: u64,
}

const EOCD_SIG: u32 = 0x0605_4b50;
const EOCD64_LOCATOR_SIG: u32 = 0x0706_4b50;
const EOCD64_SIG: u32 = 0x0606_4b50;
const CD_ENTRY_SIG: u32 = 0x0201_4b50;
const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;

const MAX_CENTRAL_DIRECTORY: u64 = 256 * 1024 * 1024;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
const METHOD_DEFLATE64: u16 = 9;

fn rd_u16(buf: &[u8], off: usize) -> Option<u64> {
    Some(u16::from_le_bytes(buf.get(off..off + 2)?.try_into().ok()?) as u64)
}
fn rd_u32(buf: &[u8], off: usize) -> Option<u64> {
    Some(u32::from_le_bytes(buf.get(off..off + 4)?.try_into().ok()?) as u64)
}
fn rd_u64(buf: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(buf.get(off..off + 8)?.try_into().ok()?))
}

fn read_central_directory<R: Read + Seek>(reader: &mut R) -> Result<Vec<CdEntry>, String> {
    let err = |what: &str| format!("central directory parse failed: {what}");
    let total = reader
        .seek(SeekFrom::End(0))
        .map_err(|e| err(&format!("seek to end: {e}")))?;

    let window = total.min(22 + 65_535);
    reader
        .seek(SeekFrom::Start(total - window))
        .map_err(|e| err(&format!("seek to tail: {e}")))?;
    let mut tail = vec![0u8; window as usize];
    reader
        .read_exact(&mut tail)
        .map_err(|e| err(&format!("read tail: {e}")))?;

    let mut eocd = None;
    for i in (0..tail.len().saturating_sub(21)).rev() {
        if rd_u32(&tail, i) != Some(EOCD_SIG as u64) {
            continue;
        }
        let comment_len = rd_u16(&tail, i + 20).unwrap_or(0) as usize;
        if i + 22 + comment_len == tail.len() {
            eocd = Some(i);
            break;
        }
        eocd.get_or_insert(i);
    }
    let eocd = eocd.ok_or_else(|| err("no end-of-central-directory signature"))?;

    let mut cd_count = rd_u16(&tail, eocd + 10).ok_or_else(|| err("truncated EOCD"))?;
    let mut cd_size = rd_u32(&tail, eocd + 12).ok_or_else(|| err("truncated EOCD"))?;
    let mut cd_offset = rd_u32(&tail, eocd + 16).ok_or_else(|| err("truncated EOCD"))?;

    if cd_count == 0xFFFF || cd_size == 0xFFFF_FFFF || cd_offset == 0xFFFF_FFFF {
        let loc = eocd
            .checked_sub(20)
            .ok_or_else(|| err("zip64 locator out of range"))?;
        if rd_u32(&tail, loc) != Some(EOCD64_LOCATOR_SIG as u64) {
            return Err(err("zip64 locator signature missing"));
        }
        let eocd64_pos = rd_u64(&tail, loc + 8).ok_or_else(|| err("truncated zip64 locator"))?;
        reader
            .seek(SeekFrom::Start(eocd64_pos))
            .map_err(|e| err(&format!("seek to zip64 EOCD: {e}")))?;
        let mut rec = [0u8; 56];
        reader
            .read_exact(&mut rec)
            .map_err(|e| err(&format!("read zip64 EOCD: {e}")))?;
        if rd_u32(&rec, 0) != Some(EOCD64_SIG as u64) {
            return Err(err("bad zip64 EOCD signature"));
        }
        cd_count = rd_u64(&rec, 32).ok_or_else(|| err("truncated zip64 EOCD"))?;
        cd_size = rd_u64(&rec, 40).ok_or_else(|| err("truncated zip64 EOCD"))?;
        cd_offset = rd_u64(&rec, 48).ok_or_else(|| err("truncated zip64 EOCD"))?;
    }

    if cd_size > MAX_CENTRAL_DIRECTORY {
        return Err(err(&format!(
            "central directory declares {cd_size} bytes, over the {MAX_CENTRAL_DIRECTORY}-byte limit"
        )));
    }
    if cd_offset.checked_add(cd_size).map(|end| end > total) != Some(false) {
        return Err(err("central directory extends past the archive"));
    }
    reader
        .seek(SeekFrom::Start(cd_offset))
        .map_err(|e| err(&format!("seek to CD: {e}")))?;
    let mut cd = vec![0u8; cd_size as usize];
    reader
        .read_exact(&mut cd)
        .map_err(|e| err(&format!("read CD: {e}")))?;

    let mut entries = Vec::with_capacity(cd_count.min(4_000_000) as usize);
    let mut records = 0u64;
    let mut pos = 0usize;
    while records < cd_count && pos + 46 <= cd.len() {
        if rd_u32(&cd, pos) != Some(CD_ENTRY_SIG as u64) {
            break;
        }
        records += 1;
        let flags = rd_u16(&cd, pos + 8).ok_or_else(|| err("truncated entry"))? as u16;
        let method = rd_u16(&cd, pos + 10).ok_or_else(|| err("truncated entry"))? as u16;
        let crc32 = rd_u32(&cd, pos + 16).ok_or_else(|| err("truncated entry"))? as u32;
        let mut csize = rd_u32(&cd, pos + 20).ok_or_else(|| err("truncated entry"))?;
        let mut size = rd_u32(&cd, pos + 24).ok_or_else(|| err("truncated entry"))?;
        let name_len = rd_u16(&cd, pos + 28).ok_or_else(|| err("truncated entry"))? as usize;
        let extra_len = rd_u16(&cd, pos + 30).ok_or_else(|| err("truncated entry"))? as usize;
        let comment_len = rd_u16(&cd, pos + 32).ok_or_else(|| err("truncated entry"))? as usize;
        let mut header_start = rd_u32(&cd, pos + 42).ok_or_else(|| err("truncated entry"))?;

        let name_start = pos + 46;
        let record_end = name_start + name_len + extra_len + comment_len;
        if record_end > cd.len() {
            return Err(err("entry record extends past the CD"));
        }
        let path =
            String::from_utf8_lossy(&cd[name_start..name_start + name_len]).replace('\\', "/");

        if size == 0xFFFF_FFFF || csize == 0xFFFF_FFFF || header_start == 0xFFFF_FFFF {
            let mut ep = name_start + name_len;
            let extra_end = ep + extra_len;
            while ep + 4 <= extra_end {
                let id = rd_u16(&cd, ep).ok_or_else(|| err("truncated extra"))?;
                let len = rd_u16(&cd, ep + 2).ok_or_else(|| err("truncated extra"))? as usize;
                if id == 0x0001 {
                    let mut fp = ep + 4;
                    if size == 0xFFFF_FFFF {
                        size = rd_u64(&cd, fp).ok_or_else(|| err("truncated zip64 extra"))?;
                        fp += 8;
                    }
                    if csize == 0xFFFF_FFFF {
                        csize = rd_u64(&cd, fp).ok_or_else(|| err("truncated zip64 extra"))?;
                        fp += 8;
                    }
                    if header_start == 0xFFFF_FFFF {
                        header_start =
                            rd_u64(&cd, fp).ok_or_else(|| err("truncated zip64 extra"))?;
                    }
                    break;
                }
                ep += 4 + len;
            }
        }

        pos = record_end;
        if path.ends_with('/') {
            continue;
        }
        entries.push(CdEntry {
            path,
            size,
            csize,
            crc32,
            method,
            flags,
            header_start,
        });
    }
    if records != cd_count {
        return Err(err(&format!(
            "truncated, parsed {records} of {cd_count} records"
        )));
    }
    if entries.is_empty() {
        return Err(err("no file entries found"));
    }
    Ok(entries)
}

pub fn manifest_from_parts(
    parts: &[PathBuf],
    packs: Vec<PackRef>,
    version: &str,
) -> Result<Manifest, String> {
    let mut reader = LocalSplitReader::open(parts)
        .map_err(|e| format!("failed to open downloaded parts: {e}"))?;
    let entries = read_central_directory(&mut reader)?;
    Ok(manifest_from_entries(packs, version, entries))
}

// ------------ Verifying Installed Files ------------
// Checks each installed file against the manifest by size and CRC and reports what is missing or damaged.
enum Problem {
    Missing,
    SizeMismatch(u64),
    CrcMismatch,
    Unreadable(io::Error),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Missing => write!(f, "is missing"),
            Problem::SizeMismatch(actual) => write!(f, "is {actual} bytes"),
            Problem::CrcMismatch => write!(f, "fails the checksum"),
            Problem::Unreadable(e) if matches!(e.raw_os_error(), Some(5) | Some(32)) => {
                write!(f, "is unreadable ({e}), is the game running?")
            }
            Problem::Unreadable(e) => write!(f, "is unreadable ({e})"),
        }
    }
}

const LOGGED_PROBLEMS: usize = 20;

pub fn verify_files(
    install_dir: &Path,
    files: &[ManifestEntry],
    hooks: &dyn Hooks,
) -> Result<Vec<ManifestEntry>, String> {
    let invalid = check_files(install_dir, files, hooks)?;
    if invalid.is_empty() {
        log::info!("verify: all {} files match the manifest.", files.len());
    } else {
        log::info!(
            "verify: {} of {} files need repair.",
            invalid.len(),
            files.len()
        );
        for (file, problem) in invalid.iter().take(LOGGED_PROBLEMS) {
            log::info!(
                "verify: {} {problem} (expected {} bytes)",
                file.path,
                file.size
            );
        }
        if invalid.len() > LOGGED_PROBLEMS {
            log::info!(
                "verify: {} more files not listed.",
                invalid.len() - LOGGED_PROBLEMS
            );
        }
    }
    Ok(invalid.into_iter().map(|(file, _)| file).collect())
}

fn check_files(
    install_dir: &Path,
    files: &[ManifestEntry],
    hooks: &dyn Hooks,
) -> Result<Vec<(ManifestEntry, Problem)>, String> {
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    hooks.event(Event::Phase {
        message: format!("Checking {} files...", files.len()),
        name: "validating",
    });
    hooks.event(Event::Totals {
        total_bytes,
        sizes: files.iter().map(|f| (f.path.clone(), f.size)).collect(),
    });

    let workers = super::perf::validation_workers().min(files.len()).max(1);
    let cursor = std::sync::atomic::AtomicUsize::new(0);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let first_error: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);
    let invalid: parking_lot::Mutex<Vec<(usize, ManifestEntry, Problem)>> =
        parking_lot::Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let mut buf = vec![0u8; IO_BUF];
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let index = cursor.fetch_add(1, Ordering::SeqCst);
                    let Some(file) = files.get(index) else {
                        break;
                    };
                    match verify_one(install_dir, file, &mut buf, hooks) {
                        Ok(None) => {}
                        Ok(Some(problem)) => invalid.lock().push((index, file.clone(), problem)),
                        Err(e) => {
                            stop.store(true, Ordering::SeqCst);
                            let mut slot = first_error.lock();
                            if slot.is_none() {
                                *slot = Some(e);
                            }
                            break;
                        }
                    }
                }
            });
        }
    });

    if let Some(e) = first_error.into_inner() {
        return Err(e);
    }
    let mut invalid = invalid.into_inner();
    invalid.sort_by_key(|(index, _, _)| *index);
    Ok(invalid
        .into_iter()
        .map(|(_, file, problem)| (file, problem))
        .collect())
}

fn verify_one(
    install_dir: &Path,
    file: &ManifestEntry,
    buf: &mut [u8],
    hooks: &dyn Hooks,
) -> Result<Option<Problem>, String> {
    super::progress::check_cancel(hooks, "Operation cancelled by user.")?;
    let path = safe_join(install_dir, &file.path)?;
    let skip = |problem: Problem| -> Result<Option<Problem>, String> {
        hooks.event(Event::Bytes {
            path: &file.path,
            delta: file.size,
            done: true,
        });
        Ok(Some(problem))
    };
    let mut handle = match std::fs::File::open(&path) {
        Ok(handle) => handle,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return skip(Problem::Missing),
        Err(e) => return skip(Problem::Unreadable(e)),
    };
    match handle.metadata() {
        Ok(meta) if meta.len() == file.size => {}
        Ok(meta) => return skip(Problem::SizeMismatch(meta.len())),
        Err(e) => return skip(Problem::Unreadable(e)),
    }

    let mut hasher = crc32fast::Hasher::new();
    let mut read_error = None;
    loop {
        super::progress::check_cancel(hooks, "Operation cancelled by user.")?;
        match handle.read(buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                hooks.event(Event::Bytes {
                    path: &file.path,
                    delta: n as u64,
                    done: false,
                });
            }
            Err(e) => {
                read_error = Some(e);
                break;
            }
        }
    }
    hooks.event(Event::Bytes {
        path: &file.path,
        delta: 0,
        done: true,
    });
    if let Some(e) = read_error {
        return Ok(Some(Problem::Unreadable(e)));
    }
    Ok((hasher.finalize() != file.crc32).then_some(Problem::CrcMismatch))
}

struct CheckingHooks<'a>(&'a dyn Hooks);

impl super::progress::Control for CheckingHooks<'_> {
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    fn wait_if_paused(&self) {
        self.0.wait_if_paused();
    }

    fn is_paused(&self) -> bool {
        self.0.is_paused()
    }
}

impl Hooks for CheckingHooks<'_> {
    fn event(&self, event: Event) {
        if !matches!(event, Event::Phase { .. }) {
            self.0.checking(event);
        }
    }
}

struct CountingReader<R> {
    inner: R,
    count: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

// ------------ Extracting Files ------------
// Pulls single files out of the packs and writes them to disk, using a few workers in parallel and retrying files
// that fail.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Extracted {
    written: u64,
    downloaded: u64,
}

fn extract_one<R: BoundedRead>(
    reader: &mut R,
    entry: &CdEntry,
    install_dir: &Path,
    hooks: &dyn Hooks,
) -> Result<Extracted, String> {
    if entry.flags & 0x1 != 0 {
        return Err(format!("'{}' is encrypted, which is unsupported", entry.path));
    }

    let final_path = safe_join(install_dir, &entry.path)?;
    if let Some(parent) = final_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            super::fs_util::fmt_io(&format!("Could not create {}", parent.display()), &e)
        })?;
    }
    let tmp_path = {
        let mut name = final_path.file_name().unwrap_or_default().to_os_string();
        name.push(format!(".{}.peebify-part", entry.header_start));
        final_path.with_file_name(name)
    };

    reader.bound(Some(
        entry
            .header_start
            .saturating_add(30 + 2 * 65_535)
            .saturating_add(entry.csize),
    ));
    reader
        .seek(SeekFrom::Start(entry.header_start))
        .map_err(|e| format!("seek to '{}': {e}", entry.path))?;
    let mut header = [0u8; 30];
    reader
        .read_exact(&mut header)
        .map_err(|e| format!("read local header of '{}': {e}", entry.path))?;
    if rd_u32(&header, 0) != Some(LOCAL_HEADER_SIG as u64) {
        return Err(format!("bad local header signature for '{}'", entry.path));
    }
    let name_len = rd_u16(&header, 26).expect("fixed-size header");
    let extra_len = rd_u16(&header, 28).expect("fixed-size header");
    reader.bound(Some(
        entry
            .header_start
            .saturating_add(30 + name_len + extra_len)
            .saturating_add(entry.csize),
    ));
    reader
        .seek(SeekFrom::Current((name_len + extra_len) as i64))
        .map_err(|e| format!("seek past header of '{}': {e}", entry.path))?;

    let consumed = std::rc::Rc::new(std::cell::Cell::new(0u64));
    let bounded = CountingReader {
        inner: reader.by_ref().take(entry.csize),
        count: std::rc::Rc::clone(&consumed),
    };
    let mut decoder: Box<dyn Read> = match entry.method {
        METHOD_STORED => Box::new(bounded),
        METHOD_DEFLATE => Box::new(flate2::read::DeflateDecoder::new(bounded)),
        METHOD_DEFLATE64 => Box::new(deflate64::Deflate64Decoder::new(bounded)),
        other => {
            return Err(format!(
                "'{}' uses unsupported compression method {other}",
                entry.path
            ))
        }
    };

    let result = (|| -> Result<u64, String> {
        let mut out = std::fs::File::create(&tmp_path)
            .map_err(|e| format!("failed to create {}: {e}", tmp_path.display()))?;
        let mut hasher = crc32fast::Hasher::new();
        let mut buf = vec![0u8; IO_BUF];
        let mut written = 0u64;
        let mut reported = 0u64;
        loop {
            super::progress::check_cancel(hooks, "Operation cancelled by user.")?;
            let n = decoder
                .read(&mut buf)
                .map_err(|e| format!("stream error for '{}': {e}", entry.path))?;
            let now = consumed.get();
            if now > reported {
                hooks.event(Event::Bytes {
                    path: &entry.path,
                    delta: now - reported,
                    done: false,
                });
                reported = now;
            }
            if n == 0 {
                break;
            }
            if written.saturating_add(n as u64) > entry.size {
                return Err(format!(
                    "'{}' decompressed to more than the {} bytes it declares",
                    entry.path, entry.size
                ));
            }
            hasher.update(&buf[..n]);
            io::Write::write_all(&mut out, &buf[..n])
                .map_err(|e| format!("write error for '{}': {e}", entry.path))?;
            written += n as u64;
        }
        io::Write::flush(&mut out).map_err(|e| e.to_string())?;
        if written != entry.size {
            return Err(format!(
                "'{}' decompressed to {written} bytes, expected {}",
                entry.path, entry.size
            ));
        }
        if hasher.finalize() != entry.crc32 {
            return Err(format!("checksum mismatch after fetching '{}'", entry.path));
        }
        Ok(written)
    })();

    let written = match result {
        Ok(written) => written,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
    };

    super::fs_util::finalize_replace(&tmp_path, &final_path)
        .map_err(|e| format!("failed to finalize {}: {e}", final_path.display()))?;
    hooks.event(Event::Bytes {
        path: &entry.path,
        delta: 0,
        done: true,
    });
    Ok(Extracted {
        written,
        downloaded: consumed.get(),
    })
}

const EXTRACT_WORKERS: usize = 6;
const ENTRY_REQUEUES: u32 = 2;
const ENTRY_RETRY_BASE_MS: u64 = 2000;

fn entry_retry_allowed(message: &str, attempt: u32) -> bool {
    use super::fs_util::FailureKind;

    if attempt >= ENTRY_REQUEUES {
        return false;
    }
    let msg = message.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| msg.contains(n));
    if has(&[
        "http 403",
        "http 404",
        "ignored the range request",
        "bad local header signature",
        "unsupported compression",
        "is encrypted",
        "failed to finalize",
        "failed to create",
        "write error",
    ]) {
        return false;
    }
    let kind = super::fs_util::classify(message);
    if matches!(
        kind,
        FailureKind::Cancelled
            | FailureKind::DiskFull
            | FailureKind::Locked
            | FailureKind::AccessDenied
    ) {
        return false;
    }
    if has(&["checksum mismatch", "decompressed to"]) {
        return attempt == 0;
    }
    matches!(kind, FailureKind::Network)
        || has(&[
            "http 5",
            "short range response",
            "range body error",
            "range request failed",
        ])
}

fn wait_before_retry(delay: Duration, control: &dyn super::progress::Control) {
    let deadline = Instant::now() + delay;
    while !control.is_cancelled() {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(200)));
    }
}

fn extract_entries_parallel(
    parts: &[PackRef],
    mut wanted: Vec<CdEntry>,
    install_dir: &Path,
    hooks: &dyn Hooks,
    cancelled: &Arc<AtomicBool>,
    handle: &tokio::runtime::Handle,
) -> Result<Extracted, String> {
    wanted.sort_by_key(|e| e.header_start);
    let workers = EXTRACT_WORKERS.min(wanted.len()).max(1);
    let queue: parking_lot::Mutex<std::collections::VecDeque<(CdEntry, u32)>> =
        parking_lot::Mutex::new(wanted.into_iter().map(|entry| (entry, 0)).collect());
    let written = std::sync::atomic::AtomicU64::new(0);
    let downloaded = std::sync::atomic::AtomicU64::new(0);
    let first_error: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);
    let stop = AtomicBool::new(false);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let open = || {
                    HttpRangeReader::open(
                        parts.to_vec(),
                        Arc::clone(cancelled),
                        handle.clone(),
                        Some(hooks),
                    )
                };
                let fail = |e: String| {
                    stop.store(true, Ordering::SeqCst);
                    let mut slot = first_error.lock();
                    if slot.is_none() {
                        *slot = Some(e);
                    }
                };
                let mut reader = match open() {
                    Ok(reader) => reader,
                    Err(e) => return fail(e),
                };
                loop {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Some((entry, attempt)) = queue.lock().pop_front() else {
                        return;
                    };
                    let outcome =
                        super::progress::check_cancel(hooks, "Operation cancelled by user.")
                            .and_then(|()| extract_one(&mut reader, &entry, install_dir, hooks));
                    match outcome {
                        Ok(extracted) => {
                            written.fetch_add(extracted.written, Ordering::SeqCst);
                            downloaded.fetch_add(extracted.downloaded, Ordering::SeqCst);
                        }
                        Err(e) if !hooks.is_cancelled() && entry_retry_allowed(&e, attempt) => {
                            log::warn!(
                                "Fetching '{}' failed ({e}), retry {}/{ENTRY_REQUEUES}",
                                entry.path,
                                attempt + 1
                            );
                            wait_before_retry(
                                super::http::retry_delay(attempt + 1, ENTRY_RETRY_BASE_MS),
                                hooks,
                            );
                            reader = match open() {
                                Ok(reader) => reader,
                                Err(e) => return fail(e),
                            };
                            queue.lock().push_back((entry, attempt + 1));
                        }
                        Err(e) => return fail(e),
                    }
                }
            });
        }
    });

    if let Some(e) = first_error.into_inner() {
        return Err(e);
    }
    Ok(Extracted {
        written: written.into_inner(),
        downloaded: downloaded.into_inner(),
    })
}

// ------------ Repair and Update ------------
// Repair re-extracts only the files that fail verification. Adopt builds a manifest for an existing install, and
// the delta update fetches only the files that changed in a new version and removes the ones that are gone.
pub struct RepairStats {
    pub validated: usize,
    pub repaired: usize,
}

const UNHOSTED: &str = "no longer hosted";
const MISMATCHED: &str = "do not match the";

pub fn is_unhosted(message: &str) -> bool {
    message.contains(UNHOSTED)
}

pub fn is_stale_packages(message: &str) -> bool {
    message.contains(UNHOSTED) || message.contains(MISMATCHED)
}

fn remote_is_gone(message: &str) -> bool {
    message.contains("no pack URLs")
        || [403u16, 404, 410]
            .iter()
            .any(|status| message.contains(&format!("HTTP {status} for ")))
}

fn remote_unreachable(message: &str) -> bool {
    let error_status = message.match_indices("HTTP ").any(|(at, _)| {
        let rest = &message.as_bytes()[at + 5..];
        rest.len() >= 8
            && rest[..3].iter().all(u8::is_ascii_digit)
            && rest[3..].starts_with(b" for ")
    });
    error_status
        || message.contains("range request failed")
        || message.contains("range body error")
        || message.contains("cancelled")
}

pub fn repair(
    install_dir: &Path,
    manifest: &Manifest,
    cancelled: Arc<AtomicBool>,
    hooks: &dyn Hooks,
    handle: tokio::runtime::Handle,
) -> Result<RepairStats, String> {
    let remote = || {
        hooks.event(Event::Phase {
            message: "Checking the hosted packages...".to_string(),
            name: "validating",
        });
        let remote = remote_entries(&manifest.packs, &cancelled, &handle, Some(hooks));
        if let Err(e) = &remote {
            log::warn!(
                "reconcile: packages for version {} are unreachable: {e}",
                manifest.version
            );
        }
        remote
    };
    repair_against(
        install_dir,
        manifest,
        remote,
        Arc::clone(&cancelled),
        hooks,
        handle.clone(),
    )
}

pub fn adopt(
    install_dir: &Path,
    packs: Vec<PackRef>,
    version: &str,
    cancelled: Arc<AtomicBool>,
    hooks: &dyn Hooks,
    handle: tokio::runtime::Handle,
) -> Result<(Manifest, RepairStats), String> {
    hooks.event(Event::Phase {
        message: "Reading the latest package index...".to_string(),
        name: "validating",
    });
    let entries = remote_entries(&packs, &cancelled, &handle, Some(hooks))?;
    let manifest = manifest_from_entries(packs, version, entries.clone());
    let stats = repair_against(
        install_dir,
        &manifest,
        move || Ok(entries),
        cancelled,
        hooks,
        handle,
    )?;
    Ok((manifest, stats))
}

fn repair_against(
    install_dir: &Path,
    manifest: &Manifest,
    remote: impl FnOnce() -> Result<Vec<CdEntry>, String>,
    cancelled: Arc<AtomicBool>,
    hooks: &dyn Hooks,
    handle: tokio::runtime::Handle,
) -> Result<RepairStats, String> {
    let invalid = verify_files(install_dir, &manifest.files, hooks)?;
    if invalid.is_empty() {
        return Ok(RepairStats {
            validated: manifest.files.len(),
            repaired: 0,
        });
    }
    let entries = remote().map_err(|e| {
        if remote_is_gone(&e) {
            format!(
                "{} files need repair, but the packages for version {} are {UNHOSTED} ({e}).",
                invalid.len(),
                manifest.version
            )
        } else if remote_unreachable(&e) {
            format!(
                "{} files need repair, but the download server could not be reached ({e}). Check your connection and try again.",
                invalid.len()
            )
        } else {
            format!(
                "{} files need repair, but the download server returned an unusable package index ({e}). Try again later.",
                invalid.len()
            )
        }
    })?;

    hooks.event(Event::Phase {
        message: format!("Repairing {} files...", invalid.len()),
        name: "repairing",
    });
    let invalid_paths: HashSet<&str> = invalid.iter().map(|f| f.path.as_str()).collect();
    let wanted: Vec<CdEntry> = entries
        .into_iter()
        .filter(|e| invalid_paths.contains(e.path.as_str()))
        .collect();
    if wanted.len() != invalid.len() {
        return Err(format!(
            "The hosted packages for version {} {MISMATCHED} {} files that need repair.",
            manifest.version,
            invalid.len()
        ));
    }

    hooks.event(Event::Totals {
        total_bytes: wanted.iter().map(|e| e.csize).sum(),
        sizes: wanted.iter().map(|e| (e.path.clone(), e.csize)).collect(),
    });
    let started = Instant::now();
    extract_entries_parallel(
        &manifest.packs,
        wanted,
        install_dir,
        hooks,
        &cancelled,
        &handle,
    )?;
    log::info!(
        "reconcile: repaired {} files in {:.1}s.",
        invalid.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(RepairStats {
        validated: manifest.files.len(),
        repaired: invalid.len(),
    })
}

pub struct DeltaStats {
    pub fetched_files: usize,
    pub fetched_bytes: u64,
    pub downloaded_bytes: u64,
    pub deleted_files: usize,
    pub total_files: usize,
}

pub fn is_structural_failure(message: &str) -> bool {
    let msg = message.to_lowercase();
    [
        "central directory parse failed",
        "no pack urls",
        "ignored the range request",
        "bad local header signature",
        "unsupported compression",
        "is encrypted",
    ]
    .iter()
    .any(|needle| msg.contains(needle))
}

fn drop_current_on_disk(
    install_dir: &Path,
    wanted: &mut Vec<CdEntry>,
    hooks: &dyn Hooks,
) -> Result<usize, String> {
    let candidates: Vec<ManifestEntry> = wanted
        .iter()
        .filter(|e| {
            safe_join(install_dir, &e.path)
                .ok()
                .and_then(|path| std::fs::metadata(path).ok())
                .is_some_and(|meta| meta.is_file() && meta.len() == e.size)
        })
        .map(|e| ManifestEntry {
            path: e.path.clone(),
            size: e.size,
            crc32: e.crc32,
        })
        .collect();
    if candidates.is_empty() {
        return Ok(0);
    }
    hooks.event(Event::Phase {
        message: format!("Checking {} files already on disk...", candidates.len()),
        name: "validating",
    });
    let stale: HashSet<String> = check_files(install_dir, &candidates, &CheckingHooks(hooks))?
        .into_iter()
        .map(|(file, _)| file.path)
        .collect();
    let current: HashSet<&str> = candidates
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| !stale.contains(*path))
        .collect();
    let before = wanted.len();
    wanted.retain(|e| !current.contains(e.path.as_str()));
    Ok(before - wanted.len())
}

fn orphans<'a>(old: &'a [ManifestEntry], new: &[CdEntry]) -> Vec<&'a ManifestEntry> {
    let keep: HashSet<String> = new.iter().map(|e| manifest_key(&e.path)).collect();
    old.iter()
        .filter(|file| !keep.contains(&manifest_key(&file.path)))
        .collect()
}

fn delete_orphans(install_dir: &Path, orphans: &[&ManifestEntry]) -> Result<usize, String> {
    let mut deleted = 0usize;
    for old_file in orphans {
        let path = safe_join(install_dir, &old_file.path)?;
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => {
                log::warn!("delta: could not read orphan {}: {e}", old_file.path);
                continue;
            }
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() != old_file.size {
            log::info!(
                "delta: keeping {} because its size no longer matches the recorded install",
                old_file.path
            );
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => deleted += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => log::warn!("delta: failed to remove orphan {}: {e}", old_file.path),
        }
    }
    Ok(deleted)
}

fn delta_space_needed(install_dir: &Path, wanted: &[CdEntry]) -> u64 {
    let on_disk = |path: &str| {
        safe_join(install_dir, path)
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .filter(|meta| meta.is_file())
            .map_or(0, |meta| meta.len())
    };
    let growth: u64 = wanted
        .iter()
        .map(|e| e.size.saturating_sub(on_disk(&e.path)))
        .sum();
    let mut sizes: Vec<u64> = wanted.iter().map(|e| e.size).collect();
    sizes.sort_unstable_by(|a, b| b.cmp(a));
    let staging: u64 = sizes.iter().take(EXTRACT_WORKERS).sum();
    growth.saturating_add(staging)
}

fn explain_delta_disk_full(install_dir: &Path, message: String) -> String {
    if super::fs_util::classify(&message) != super::fs_util::FailureKind::DiskFull
        || message.starts_with("Not enough disk space")
    {
        return message;
    }
    format!(
        "Not enough disk space in {}. Free up space and try again. ({message})",
        install_dir.display()
    )
}

pub fn delta_update(
    install_dir: &Path,
    old_manifest: &Manifest,
    new_packs: Vec<PackRef>,
    new_version: &str,
    cancelled: Arc<AtomicBool>,
    hooks: &dyn Hooks,
    handle: tokio::runtime::Handle,
) -> Result<DeltaStats, String> {
    hooks.event(Event::Phase {
        message: "Reading update manifest...".to_string(),
        name: "fetching-index",
    });
    let new_entries = remote_entries(&new_packs, &cancelled, &handle, Some(hooks))?;

    let old_by_path: HashMap<String, (u64, u32)> = old_manifest
        .files
        .iter()
        .map(|f| (manifest_key(&f.path), (f.size, f.crc32)))
        .collect();

    let mut wanted: Vec<CdEntry> = new_entries
        .iter()
        .filter(|e| old_by_path.get(&manifest_key(&e.path)) != Some(&(e.size, e.crc32)))
        .cloned()
        .collect();
    let changed_files = wanted.len();
    let reused = drop_current_on_disk(install_dir, &mut wanted, hooks)?;

    let fetched_files = wanted.len();
    let fetched_bytes_expected: u64 = wanted.iter().map(|e| e.size).sum();
    let download_bytes_expected: u64 = wanted.iter().map(|e| e.csize).sum();
    log::info!(
        "Endfield delta {} -> {new_version}: {changed_files} of {} files changed, {reused} already current on disk, fetching {fetched_files} ({:.2}GB download, {:.2}GB unpacked of {:.2}GB)",
        old_manifest.version,
        new_entries.len(),
        super::progress::gib(download_bytes_expected as f64),
        super::progress::gib(fetched_bytes_expected as f64),
        super::progress::gib(new_entries.iter().map(|e| e.size).sum::<u64>() as f64),
    );

    super::download_engine::ensure_disk_space(
        install_dir,
        delta_space_needed(install_dir, &wanted),
        1.0,
        super::download_engine::HEADROOM_INSTALL,
    )?;

    hooks.event(Event::Phase {
        message: format!("Downloading {fetched_files} changed files..."),
        name: "downloading",
    });
    hooks.event(Event::Totals {
        total_bytes: download_bytes_expected,
        sizes: wanted.iter().map(|e| (e.path.clone(), e.csize)).collect(),
    });
    let extracted =
        extract_entries_parallel(&new_packs, wanted, install_dir, hooks, &cancelled, &handle)
            .map_err(|e| explain_delta_disk_full(install_dir, e))?;

    let deleted_files = delete_orphans(install_dir, &orphans(&old_manifest.files, &new_entries))?;

    let manifest = manifest_from_entries(new_packs, new_version, new_entries);
    let total_files = manifest.files.len();
    save_manifest(install_dir, &manifest)?;

    Ok(DeltaStats {
        fetched_files,
        fetched_bytes: extracted.written,
        downloaded_bytes: extracted.downloaded,
        deleted_files,
        total_files,
    })
}

pub fn latest_packs(latest: &serde_json::Value) -> Option<(String, Vec<PackRef>)> {
    let version = latest["version"].as_str().filter(|v| !v.is_empty())?;
    let resources = super::hypergryph::packs_as_resources(&latest["packs"]);
    let packs = packs_from_resources(&resources);
    if packs.is_empty() || packs.len() != resources.len() {
        return None;
    }
    Some((version.to_string(), packs))
}

pub fn packs_from_resources(resources: &[super::validator::Resource]) -> Vec<PackRef> {
    let mut packs: Vec<PackRef> = resources
        .iter()
        .filter_map(|r| {
            let url = r.url()?.to_string();
            Some(PackRef { url, size: r.size })
        })
        .collect();
    packs.sort_by(|a, b| a.url.cmp(&b.url));
    packs
}

// ------------ Package Installer Tests ------------
// Unit tests for the zip index reader, the split reader, extraction, orphan cleanup and file checks.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    impl BoundedRead for Cursor<Vec<u8>> {}

    struct NoHooks;

    impl super::super::progress::Control for NoHooks {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    impl Hooks for NoHooks {
        fn event(&self, _event: Event) {}
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "peebify-reconcile-test-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn put16(out: &mut Vec<u8>, v: u16) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn put32(out: &mut Vec<u8>, v: u32) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn put64(out: &mut Vec<u8>, v: u64) {
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn build_zip(items: &[(&str, &[u8])], zip64: bool, comment: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in items {
            let offset = out.len() as u32;
            let crc = crc32fast::hash(data);
            put32(&mut out, LOCAL_HEADER_SIG);
            put16(&mut out, 20);
            put16(&mut out, 0);
            put16(&mut out, METHOD_STORED);
            put32(&mut out, 0);
            put32(&mut out, crc);
            put32(&mut out, data.len() as u32);
            put32(&mut out, data.len() as u32);
            put16(&mut out, name.len() as u16);
            put16(&mut out, 0);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);

            put32(&mut central, CD_ENTRY_SIG);
            put16(&mut central, 20);
            put16(&mut central, 20);
            put16(&mut central, 0);
            put16(&mut central, METHOD_STORED);
            put32(&mut central, 0);
            put32(&mut central, crc);
            put32(&mut central, data.len() as u32);
            put32(&mut central, data.len() as u32);
            put16(&mut central, name.len() as u16);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put16(&mut central, 0);
            put32(&mut central, 0);
            put32(&mut central, offset);
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u64;
        let cd_size = central.len() as u64;
        out.extend_from_slice(&central);
        if zip64 {
            let eocd64_pos = out.len() as u64;
            put32(&mut out, EOCD64_SIG);
            put64(&mut out, 44);
            put16(&mut out, 45);
            put16(&mut out, 45);
            put32(&mut out, 0);
            put32(&mut out, 0);
            put64(&mut out, items.len() as u64);
            put64(&mut out, items.len() as u64);
            put64(&mut out, cd_size);
            put64(&mut out, cd_offset);
            put32(&mut out, EOCD64_LOCATOR_SIG);
            put32(&mut out, 0);
            put64(&mut out, eocd64_pos);
            put32(&mut out, 1);
        }
        put32(&mut out, EOCD_SIG);
        put16(&mut out, 0);
        put16(&mut out, 0);
        let count = if zip64 { 0xFFFF } else { items.len() as u16 };
        put16(&mut out, count);
        put16(&mut out, count);
        put32(&mut out, if zip64 { 0xFFFF_FFFF } else { cd_size as u32 });
        put32(&mut out, if zip64 { 0xFFFF_FFFF } else { cd_offset as u32 });
        put16(&mut out, comment.len() as u16);
        out.extend_from_slice(comment);
        out
    }

    fn sample() -> Vec<(&'static str, &'static [u8])> {
        vec![
            ("Data/", b""),
            ("Data/a.bin", b"alpha contents"),
            ("Data/b.bin", b"bravo"),
            ("root.txt", b"charlie charlie charlie"),
        ]
    }

    fn cd_entry(path: &str) -> CdEntry {
        CdEntry {
            path: path.to_string(),
            size: 1,
            csize: 1,
            crc32: 0,
            method: METHOD_STORED,
            flags: 0,
            header_start: 0,
        }
    }

    fn manifest_entry(path: &str, data: &[u8]) -> ManifestEntry {
        ManifestEntry {
            path: path.to_string(),
            size: data.len() as u64,
            crc32: crc32fast::hash(data),
        }
    }

    #[test]
    fn central_directory_lists_files_and_skips_directories() {
        let zip = build_zip(&sample(), false, b"");
        let entries = read_central_directory(&mut Cursor::new(zip)).unwrap();
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["Data/a.bin", "Data/b.bin", "root.txt"]);
        assert_eq!(entries[0].size, 14);
        assert_eq!(entries[0].crc32, crc32fast::hash(b"alpha contents"));
    }

    #[test]
    fn central_directory_reads_zip64_locator() {
        let zip = build_zip(&sample(), true, b"");
        let entries = read_central_directory(&mut Cursor::new(zip)).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[2].path, "root.txt");
    }

    #[test]
    fn central_directory_rejects_a_corrupt_record() {
        let mut zip = build_zip(&sample(), false, b"");
        let cd_start = zip
            .windows(4)
            .position(|w| w == CD_ENTRY_SIG.to_le_bytes())
            .unwrap();
        let second = cd_start + 46 + "Data/".len();
        assert_eq!(&zip[second..second + 4], &CD_ENTRY_SIG.to_le_bytes());
        zip[second] = 0;
        let err = read_central_directory(&mut Cursor::new(zip)).unwrap_err();
        assert!(err.contains("parsed 1 of 4 records"), "{err}");
    }

    #[test]
    fn central_directory_rejects_an_oversized_size() {
        let mut zip = build_zip(&sample(), false, b"");
        let size_at = zip.len() - 22 + 12;
        zip[size_at..size_at + 4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
        let err = read_central_directory(&mut Cursor::new(zip)).unwrap_err();
        assert!(err.contains("over the"), "{err}");
    }

    #[test]
    fn central_directory_ignores_signature_inside_comment() {
        let mut comment = b"note ".to_vec();
        comment.extend_from_slice(&EOCD_SIG.to_le_bytes());
        comment.extend_from_slice(&[0xAB; 30]);
        let zip = build_zip(&sample(), false, &comment);
        let entries = read_central_directory(&mut Cursor::new(zip)).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn split_reader_reads_across_part_boundaries() {
        let temp = TempDir::new("split");
        let zip = build_zip(&sample(), false, b"");
        let cuts = [0, 7, 40, zip.len()];
        let parts: Vec<PathBuf> = cuts
            .windows(2)
            .enumerate()
            .map(|(i, w)| {
                let path = temp.0.join(format!("game.zip.{:03}", i + 1));
                std::fs::write(&path, &zip[w[0]..w[1]]).unwrap();
                path
            })
            .collect();
        let mut reader = LocalSplitReader::open(&parts).unwrap();
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        assert_eq!(all, zip);
        reader.seek(SeekFrom::Start(5)).unwrap();
        let mut span = [0u8; 10];
        reader.read_exact(&mut span).unwrap();
        assert_eq!(span, zip[5..15]);
        let entries = read_central_directory(&mut reader).unwrap();
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn resolve_seek_rejects_negative_targets() {
        assert_eq!(resolve_seek(10, 100, SeekFrom::Current(-4)).unwrap(), 6);
        assert_eq!(resolve_seek(10, 100, SeekFrom::End(-1)).unwrap(), 99);
        assert!(resolve_seek(3, 100, SeekFrom::Current(-4)).is_err());
    }

    #[test]
    fn extract_one_writes_the_entry() {
        let temp = TempDir::new("extract");
        let zip = build_zip(&sample(), false, b"");
        let mut reader = Cursor::new(zip);
        let entries = read_central_directory(&mut reader).unwrap();
        let extracted = extract_one(&mut reader, &entries[2], &temp.0, &NoHooks).unwrap();
        assert_eq!(
            extracted,
            Extracted {
                written: 23,
                downloaded: 23
            }
        );
        assert_eq!(
            std::fs::read(temp.0.join("root.txt")).unwrap(),
            b"charlie charlie charlie"
        );
    }

    struct RecordingHooks(parking_lot::Mutex<Vec<(u64, bool)>>);

    impl super::super::progress::Control for RecordingHooks {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    impl Hooks for RecordingHooks {
        fn event(&self, event: Event) {
            if let Event::Bytes { delta, done, .. } = event {
                self.0.lock().push((delta, done));
            }
        }
    }

    #[test]
    fn extract_one_reports_compressed_bytes_as_progress() {
        let temp = TempDir::new("deflate");
        let data = b"delta delta delta delta ".repeat(512);
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        io::Write::write_all(&mut encoder, &data).unwrap();
        let packed = encoder.finish().unwrap();
        let name = "packed.bin";
        let crc = crc32fast::hash(&data);
        let mut zip = Vec::new();
        put32(&mut zip, LOCAL_HEADER_SIG);
        put16(&mut zip, 20);
        put16(&mut zip, 0);
        put16(&mut zip, METHOD_DEFLATE);
        put32(&mut zip, 0);
        put32(&mut zip, crc);
        put32(&mut zip, packed.len() as u32);
        put32(&mut zip, data.len() as u32);
        put16(&mut zip, name.len() as u16);
        put16(&mut zip, 0);
        zip.extend_from_slice(name.as_bytes());
        zip.extend_from_slice(&packed);
        let entry = CdEntry {
            path: name.to_string(),
            size: data.len() as u64,
            csize: packed.len() as u64,
            crc32: crc,
            method: METHOD_DEFLATE,
            flags: 0,
            header_start: 0,
        };
        let hooks = RecordingHooks(parking_lot::Mutex::new(Vec::new()));

        let extracted = extract_one(&mut Cursor::new(zip), &entry, &temp.0, &hooks).unwrap();

        assert!(packed.len() < data.len());
        assert_eq!(
            extracted,
            Extracted {
                written: data.len() as u64,
                downloaded: packed.len() as u64
            }
        );
        let events = hooks.0.into_inner();
        let streamed: u64 = events
            .iter()
            .filter(|(_, done)| !done)
            .map(|(delta, _)| delta)
            .sum();
        assert_eq!(streamed, packed.len() as u64);
        assert_eq!(events.last(), Some(&(0, true)));
        assert_eq!(std::fs::read(temp.0.join(name)).unwrap(), data);
    }

    #[test]
    fn entry_totals_sum_uncompressed_sizes() {
        let mut big = cd_entry("big.bin");
        big.size = 1_000;
        big.csize = 10;
        let entries = vec![cd_entry("a.bin"), big];
        assert_eq!(entry_totals(&entries), (1_001, 2));
        assert_eq!(entry_totals(&[]), (0, 0));
    }

    #[test]
    fn range_end_respects_the_entry_bound() {
        assert_eq!(range_end(0, 256, 10_000, None), 256);
        assert_eq!(range_end(9_900, 256, 10_000, None), 10_000);
        assert_eq!(range_end(100, 8_000, 10_000, Some(1_000)), 1_000);
        assert_eq!(range_end(1_000, 8_000, 10_000, Some(1_000)), 1_001);
    }

    #[test]
    fn orphans_ignore_case_only_renames() {
        let old = vec![
            manifest_entry("Foo/Bar.dll", b"x"),
            manifest_entry("gone.pak", b"y"),
        ];
        let new = vec![cd_entry("foo/Bar.dll"), cd_entry("kept.pak")];
        let removed: Vec<&str> = orphans(&old, &new)
            .into_iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(removed, ["gone.pak"]);
    }

    #[test]
    fn orphans_ignore_separator_only_respellings() {
        let old = vec![manifest_entry(r"Foo\Bar.dll", b"x")];
        let new = vec![cd_entry("/foo/./bar.DLL")];
        assert!(orphans(&old, &new).is_empty());
    }

    #[test]
    fn orphan_deletion_keeps_a_file_whose_size_changed() {
        let temp = TempDir::new("orphan-size");
        std::fs::write(temp.0.join("gone.pak"), b"y").unwrap();
        std::fs::write(temp.0.join("grown.pak"), b"grown since install").unwrap();
        let gone = manifest_entry("gone.pak", b"y");
        let grown = manifest_entry("grown.pak", b"z");
        let missing = manifest_entry("missing.pak", b"m");

        let deleted = delete_orphans(&temp.0, &[&gone, &grown, &missing]).unwrap();

        assert_eq!(deleted, 1);
        assert!(!temp.0.join("gone.pak").exists());
        assert!(temp.0.join("grown.pak").is_file());
    }

    #[test]
    fn check_files_reports_each_problem() {
        let temp = TempDir::new("check");
        std::fs::write(temp.0.join("good.bin"), b"good").unwrap();
        std::fs::write(temp.0.join("short.bin"), b"sh").unwrap();
        std::fs::write(temp.0.join("bad.bin"), b"bads").unwrap();
        let files = vec![
            manifest_entry("good.bin", b"good"),
            manifest_entry("short.bin", b"short"),
            manifest_entry("bad.bin", b"good"),
            manifest_entry("missing.bin", b"none"),
        ];
        let invalid = check_files(&temp.0, &files, &NoHooks).unwrap();
        let found: Vec<(&str, String)> = invalid
            .iter()
            .map(|(f, p)| (f.path.as_str(), p.to_string()))
            .collect();
        assert_eq!(
            found,
            [
                ("short.bin", "is 2 bytes".to_string()),
                ("bad.bin", "fails the checksum".to_string()),
                ("missing.bin", "is missing".to_string()),
            ]
        );
    }

    #[test]
    fn delta_resume_skips_files_already_current() {
        let temp = TempDir::new("resume");
        std::fs::write(temp.0.join("done.bin"), b"fresh").unwrap();
        std::fs::write(temp.0.join("stale.bin"), b"older").unwrap();
        let entry = |path: &str, data: &[u8]| CdEntry {
            path: path.to_string(),
            size: data.len() as u64,
            csize: data.len() as u64,
            crc32: crc32fast::hash(data),
            method: METHOD_STORED,
            flags: 0,
            header_start: 0,
        };
        let mut wanted = vec![
            entry("done.bin", b"fresh"),
            entry("stale.bin", b"newer"),
            entry("absent.bin", b"new"),
        ];
        let reused = drop_current_on_disk(&temp.0, &mut wanted, &NoHooks).unwrap();
        assert_eq!(reused, 1);
        let left: Vec<&str> = wanted.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(left, ["stale.bin", "absent.bin"]);
    }

    type RangeLog = Arc<parking_lot::Mutex<Vec<(usize, u64, u64)>>>;

    fn range_server(parts: Vec<Vec<u8>>, cut: usize) -> (String, RangeLog) {
        use std::io::{BufRead, BufReader, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let parts = Arc::new(parts);
        let served_log = Arc::clone(&log);
        std::thread::spawn(move || {
            let cut_once = Arc::new(AtomicBool::new(false));
            for conn in listener.incoming() {
                let Ok(mut conn) = conn else { continue };
                let (parts, log, cut_once) =
                    (Arc::clone(&parts), Arc::clone(&served_log), Arc::clone(&cut_once));
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(conn.try_clone().unwrap());
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let part: usize = line.split(' ').nth(1).unwrap()[1..].parse().unwrap();
                    let mut range = (0u64, 0u64);
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).unwrap();
                        if header.trim().is_empty() {
                            break;
                        }
                        let lower = header.to_ascii_lowercase();
                        if let Some(spec) = lower.strip_prefix("range: bytes=") {
                            let (a, b) = spec.trim().split_once('-').unwrap();
                            range = (a.parse().unwrap(), b.parse().unwrap());
                        }
                    }
                    log.lock().push((part, range.0, range.1 + 1));
                    let data = &parts[part];
                    let body = &data[range.0 as usize..=range.1 as usize];
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nConnection: close\r\n\r\n",
                        body.len(),
                        range.0,
                        range.1,
                        data.len()
                    );
                    let _ = conn.write_all(head.as_bytes());
                    let streamed = body.len() as u64 > REMOTE_PROBE_CHUNK;
                    if streamed && !cut_once.swap(true, Ordering::SeqCst) {
                        let _ = conn.write_all(&body[..cut.min(body.len())]);
                        let _ = conn.flush();
                        let _ = conn.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    let _ = conn.write_all(body);
                });
            }
        });
        (base, log)
    }

    #[test]
    fn remote_reader_streams_across_parts_and_resumes_after_a_cut() {
        let data: Vec<u8> = (0..4_500_000u32).map(|i| (i % 251) as u8).collect();
        let split = 3_000_000usize;
        let cut = 1_200_000usize;
        let (base, log) = range_server(vec![data[..split].to_vec(), data[split..].to_vec()], cut);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let packs = vec![
            PackRef {
                url: format!("{base}/0"),
                size: split as u64,
            },
            PackRef {
                url: format!("{base}/1"),
                size: (data.len() - split) as u64,
            },
        ];
        let mut reader = HttpRangeReader::open(
            packs,
            Arc::new(AtomicBool::new(false)),
            runtime.handle().clone(),
            None,
        )
        .unwrap();
        reader.seek(SeekFrom::Start(100)).unwrap();
        let mut out = vec![0u8; data.len() - 100];
        reader.read_exact(&mut out).unwrap();
        assert!(out == data[100..], "streamed bytes differ from the source");

        let log = log.lock().clone();
        let probe_end = 100 + REMOTE_PROBE_CHUNK;
        assert_eq!(log[0], (0, 100, probe_end));
        assert_eq!(log[1], (0, probe_end, split as u64));
        assert_eq!(log[2], (0, probe_end + cut as u64, split as u64));
        assert_eq!(log[3], (1, 0, (data.len() - split) as u64));
        assert_eq!(log.len(), 4);
    }

    #[test]
    fn delta_resume_reports_its_check_as_checking_progress() {
        #[derive(Default)]
        struct Recorder {
            phases: parking_lot::Mutex<Vec<String>>,
            event_bytes: std::sync::atomic::AtomicU64,
            checked_total: std::sync::atomic::AtomicU64,
            checked_bytes: std::sync::atomic::AtomicU64,
        }
        impl super::super::progress::Control for Recorder {
            fn is_cancelled(&self) -> bool {
                false
            }
        }
        impl Hooks for Recorder {
            fn event(&self, event: Event) {
                match event {
                    Event::Phase { message, .. } => self.phases.lock().push(message),
                    Event::Totals { .. } => panic!("check totals reached the download counters"),
                    Event::Bytes { delta, .. } => {
                        self.event_bytes.fetch_add(delta, Ordering::SeqCst);
                    }
                }
            }
            fn checking(&self, event: Event) {
                match event {
                    Event::Phase { .. } => panic!("check_files phase was forwarded"),
                    Event::Totals { total_bytes, .. } => {
                        self.checked_total.store(total_bytes, Ordering::SeqCst);
                    }
                    Event::Bytes { delta, .. } => {
                        self.checked_bytes.fetch_add(delta, Ordering::SeqCst);
                    }
                }
            }
        }

        let temp = TempDir::new("resume-progress");
        std::fs::write(temp.0.join("done.bin"), b"fresh").unwrap();
        std::fs::write(temp.0.join("stale.bin"), b"older").unwrap();
        let mut wanted: Vec<CdEntry> = [("done.bin", b"fresh"), ("stale.bin", b"newer")]
            .iter()
            .map(|(path, data)| CdEntry {
                size: data.len() as u64,
                crc32: crc32fast::hash(*data),
                ..cd_entry(path)
            })
            .collect();
        let hooks = Recorder::default();
        assert_eq!(drop_current_on_disk(&temp.0, &mut wanted, &hooks).unwrap(), 1);
        assert_eq!(
            *hooks.phases.lock(),
            ["Checking 2 files already on disk...".to_string()]
        );
        assert_eq!(hooks.event_bytes.load(Ordering::SeqCst), 0);
        assert_eq!(hooks.checked_total.load(Ordering::SeqCst), 10);
        assert_eq!(hooks.checked_bytes.load(Ordering::SeqCst), 10);
    }

    #[test]
    fn manifest_round_trips_and_replaces_existing() {
        let temp = TempDir::new("manifest");
        let mut manifest = manifest_from_entries(
            vec![PackRef {
                url: "https://cdn.example/game.zip.001".to_string(),
                size: 10,
            }],
            "1.0.0",
            vec![cd_entry("a.bin")],
        );
        save_manifest(&temp.0, &manifest).unwrap();
        manifest.version = "1.0.1".to_string();
        save_manifest(&temp.0, &manifest).unwrap();
        let loaded = load_manifest(&temp.0).unwrap();
        assert_eq!(loaded.version, "1.0.1");
        assert_eq!(loaded.files.len(), 1);
        assert!(!temp.0.join(format!("{MANIFEST_FILE}.tmp")).exists());
        std::fs::write(temp.0.join(MANIFEST_FILE), "{not json").unwrap();
        assert!(load_manifest(&temp.0).is_none());
    }

    #[test]
    fn entry_retry_only_covers_transient_failures() {
        assert!(entry_retry_allowed(
            "stream error for 'a': range request failed: connection reset",
            0
        ));
        assert!(entry_retry_allowed(
            "read local header of 'a': HTTP 503 for https://cdn/x",
            1
        ));
        assert!(!entry_retry_allowed(
            "read local header of 'a': HTTP 503 for https://cdn/x",
            ENTRY_REQUEUES
        ));
        assert!(!entry_retry_allowed("stream error for 'a': HTTP 404 for u", 0));
        assert!(!entry_retry_allowed(
            "stream error for 'a': CDN ignored the Range request (returned 200)",
            0
        ));
        assert!(!entry_retry_allowed("Operation cancelled by user.", 0));
        assert!(entry_retry_allowed("checksum mismatch after fetching 'a'", 0));
        assert!(!entry_retry_allowed("checksum mismatch after fetching 'a'", 1));
        assert!(!entry_retry_allowed("failed to finalize C:/x: locked", 0));
    }

    #[test]
    fn structural_failures_are_recognised() {
        assert!(is_structural_failure(
            "central directory parse failed: truncated, parsed 1 of 4 records"
        ));
        assert!(is_structural_failure(
            "stream error for 'a': CDN ignored the Range request (returned 200)"
        ));
        assert!(!is_structural_failure(
            "stream error for 'a': range request failed: timed out"
        ));
        assert!(!is_structural_failure("HTTP 403 for https://cdn/x"));
    }

    #[test]
    fn only_a_gone_answer_counts_as_unhosted() {
        let temp = TempDir::new("unhosted");
        let manifest = manifest_from_entries(
            vec![PackRef {
                url: "https://cdn.example/game.zip.001".to_string(),
                size: 10,
            }],
            "1.0.0",
            vec![cd_entry("missing.bin")],
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let repair = |remote: &str| {
            let remote = remote.to_string();
            repair_against(
                &temp.0,
                &manifest,
                move || Err(remote),
                Arc::new(AtomicBool::new(false)),
                &NoHooks,
                runtime.handle().clone(),
            )
            .err()
            .unwrap()
        };

        for gone in ["404", "403", "410"] {
            let e = repair(&format!(
                "central directory parse failed: read tail: HTTP {gone} for https://cdn/x"
            ));
            assert!(is_unhosted(&e), "{e}");
        }
        for offline in [
            "central directory parse failed: read tail: range request failed: dns error",
            "central directory parse failed: read tail: HTTP 503 for https://cdn/x",
            "central directory parse failed: read tail: range body error: timed out",
        ] {
            let e = repair(offline);
            assert!(!is_stale_packages(&e), "{e}");
            assert!(e.contains("could not be reached"), "{e}");
        }
        for unusable in [
            "central directory parse failed: no end-of-central-directory signature",
            "central directory parse failed: read tail: CDN ignored the Range request (returned 200)",
        ] {
            let e = repair(unusable);
            assert!(!is_stale_packages(&e), "{e}");
            assert!(e.contains("unusable package index"), "{e}");
        }

        let e = repair_against(
            &temp.0,
            &manifest,
            || Ok(vec![cd_entry("elsewhere.bin")]),
            Arc::new(AtomicBool::new(false)),
            &NoHooks,
            runtime.handle().clone(),
        )
        .err()
        .unwrap();
        assert!(is_stale_packages(&e), "{e}");

        let never = || -> Result<Vec<CdEntry>, String> {
            panic!("read the hosted index with nothing to repair")
        };
        let intact = manifest_from_entries(manifest.packs.clone(), "1.0.0", Vec::new());
        let stats = repair_against(
            &temp.0,
            &intact,
            never,
            Arc::new(AtomicBool::new(false)),
            &NoHooks,
            runtime.handle().clone(),
        )
        .unwrap();
        assert_eq!(stats.repaired, 0);
    }

    #[test]
    fn delta_space_counts_growth_and_staging() {
        let temp = TempDir::new("delta-space");
        std::fs::write(temp.0.join("grown.bin"), vec![0u8; 40]).unwrap();
        std::fs::write(temp.0.join("shrunk.bin"), vec![0u8; 90]).unwrap();
        let entry = |path: &str, size: u64| CdEntry {
            size,
            ..cd_entry(path)
        };
        let wanted = vec![
            entry("grown.bin", 100),
            entry("shrunk.bin", 50),
            entry("new.bin", 30),
        ];
        assert_eq!(delta_space_needed(&temp.0, &wanted), 90 + 180);
        let many: Vec<CdEntry> = (0..EXTRACT_WORKERS as u64 + 2)
            .map(|i| entry(&format!("f{i}.bin"), i + 1))
            .collect();
        let growth: u64 = many.iter().map(|e| e.size).sum();
        let largest: u64 = (3..=EXTRACT_WORKERS as u64 + 2).sum();
        assert_eq!(delta_space_needed(&temp.0, &many), growth + largest);
    }

    #[test]
    fn delta_disk_full_is_explained_once() {
        let dir = Path::new("C:/Games/Endfield");
        let raw = "write error for 'a': There is not enough space on the disk. (os error 112)";
        let explained = explain_delta_disk_full(dir, raw.to_string());
        assert!(explained.starts_with("Not enough disk space in "), "{explained}");
        assert!(explained.contains(raw));
        assert_eq!(
            super::super::fs_util::classify(&explained),
            super::super::fs_util::FailureKind::DiskFull
        );
        assert_eq!(explain_delta_disk_full(dir, explained.clone()), explained);
        let other = "range request failed: timed out".to_string();
        assert_eq!(explain_delta_disk_full(dir, other.clone()), other);
    }
}
