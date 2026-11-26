//! Audio on disk. One file per minute; inside it, one sealed 100 ms block after another.
//!
//! A sealed block is AES-256-GCM over the block's samples, and its nonce is its position:
//! the minute, and the index within the minute. Sealed blocks are all the same size, so
//! position is just offset. Nothing can be reordered or spliced in, and a crash can only
//! leave a short tail on the file that was being written.

use crate::capture::clock::{BLOCK, Block};
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zutalk_log::warn;

/// Duration of one audio block on the capture clock (100 ms, matching `clock::BLOCK` at 16 kHz).
pub const BLOCK_MS: u32 = 100;
/// Duration of one minute file on disk (60 000 ms = 600 blocks).
const MINUTE_MS: u32 = 60_000;
/// Bytes of one sealed block: the samples, plus the authentication tag.
const SEALED: usize = BLOCK * 2 + 16;
/// Pre-zeroed PCM payload for padding a hole on disk with sealed silence without allocating.
const SILENT_PLAIN: [u8; BLOCK * 2] = [0; BLOCK * 2];

/// How an incoming block's timestamp relates to the next block expected on disk (`P1.md` §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Earlier than what is already written (e.g. written before a restart): skip it.
    Before,
    /// Exactly the next expected block: write it directly.
    Next,
    /// Ahead of the next expected block: pad `blocks` silent blocks first, then write it.
    Ahead { blocks: u32 },
}

/// Pure classification of `block_ms` against the writer's next expected timestamp `next_ms`.
pub fn place(next_ms: u32, block_ms: u32) -> Place {
    if block_ms < next_ms {
        Place::Before
    } else {
        let blocks = (block_ms - next_ms) / BLOCK_MS;
        if blocks == 0 { Place::Next } else { Place::Ahead { blocks } }
    }
}

/// Audio leaves this module only for a stated purpose. There is no other way to read it.
pub enum Pass {
    /// To transcribe the recording after it has ended.
    Write,
    /// To export the audio because the person asked for it.
    Export,
    /// To file the recording with its audio in a city's archive, the one place audio may stay.
    City,
}

fn file(dir: &Path, minute: u32) -> PathBuf {
    dir.join(format!("{minute:05}.tape"))
}

fn open(dir: &Path, minute: u32) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(file(dir, minute))
}

fn nonce(minute: u32, index: u32) -> [u8; 12] {
    let mut n = [0; 12];
    n[..4].copy_from_slice(&minute.to_be_bytes());
    n[8..].copy_from_slice(&index.to_be_bytes());
    n
}

fn bad(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

pub struct Writer {
    dir: PathBuf,
    cipher: Aes256Gcm,
    minute: u32,
    file: File,
    next_ms: u32,
    dirty: bool,
    padded_since: u64,
}

impl Writer {
    /// `key` belongs to this recording alone: positions repeat across recordings.
    /// When `dir` already holds minute files, computes the next expected block timestamp from file
    /// sizes without decrypting and truncates any partial tail block on the last minute file.
    pub fn create(dir: &Path, key: &[u8; 32]) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let existing = minutes(dir)?;
        let (minute, next_ms) = if let Some(&last) = existing.last() {
            for &earlier in &existing[..existing.len() - 1] {
                let len = fs::metadata(file(dir, earlier))?.len();
                if len % SEALED as u64 != 0 {
                    return Err(bad("a short block inside the recording"));
                }
            }
            let path = file(dir, last);
            let len = fs::metadata(&path)?.len();
            let whole = len / SEALED as u64 * SEALED as u64;
            if whole < len {
                OpenOptions::new().write(true).open(&path)?.set_len(whole)?;
            }
            let blocks_in_last = (whole / SEALED as u64) as u32;
            (last, last * MINUTE_MS + blocks_in_last * BLOCK_MS)
        } else {
            (0, 0)
        };
        Ok(Self {
            dir: dir.into(),
            cipher: Aes256Gcm::new(key.into()),
            minute,
            file: open(dir, minute)?,
            next_ms,
            dirty: false,
            padded_since: 0,
        })
    }

    fn write_plain(&mut self, t_ms: u32, plain: &[u8]) -> io::Result<()> {
        let (minute, index) = (t_ms / MINUTE_MS, t_ms % MINUTE_MS / BLOCK_MS);
        if minute != self.minute {
            // A finished minute is made durable. The current one rides the page cache:
            // syncing ten times a second is far too slow on macOS.
            self.file.sync_data()?;
            self.dirty = false;
            self.file = open(&self.dir, minute)?;
            self.minute = minute;
        }
        let sealed = self.cipher.encrypt(&nonce(minute, index).into(), plain);
        self.file.write_all(&sealed.map_err(|_| bad("block does not seal"))?)?;
        self.dirty = true;
        self.next_ms = t_ms + BLOCK_MS;
        Ok(())
    }

    /// Appends `block`: skips blocks before `next_ms`, writes exact successors directly, and pads
    /// any hole ahead of `next_ms` with sealed silence across minute boundaries.
    pub fn append(&mut self, block: &Block) -> io::Result<()> {
        match place(self.next_ms, block.t_ms) {
            Place::Before => return Ok(()),
            Place::Next => {}
            Place::Ahead { blocks } => {
                let from_ms = self.next_ms;
                for i in 0..blocks {
                    self.write_plain(from_ms + i * BLOCK_MS, &SILENT_PLAIN)?;
                    self.padded_since += 1;
                }
                warn!("tape.pad", from_ms = from_ms, blocks = blocks);
            }
        }
        let plain: Vec<u8> = block.pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.write_plain(block.t_ms, &plain)
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()?;
        self.dirty = false;
        Ok(())
    }

    /// Silent blocks padded by this writer since the last call, resetting the tally.
    pub fn take_padded(&mut self) -> u64 {
        std::mem::take(&mut self.padded_since)
    }
}

/// A tape being written. Dropped, it is on the disk.
pub trait Reel: Send {
    fn append(&mut self, block: &Block) -> io::Result<()>;

    /// Flushes buffered audio to disk. Explicit close calls this so errors are reported.
    fn sync(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Silent blocks padded by this reel since the last call, resetting the tally.
    fn take_padded(&mut self) -> u64 {
        0
    }
}

impl Reel for Writer {
    fn append(&mut self, block: &Block) -> io::Result<()> {
        Writer::append(self, block)
    }

    fn sync(&mut self) -> io::Result<()> {
        Writer::sync(self)
    }

    fn take_padded(&mut self) -> u64 {
        Writer::take_padded(self)
    }
}

/// Opens a recording's tape for writing. Reading audio still goes only through `tape::Pass`.
pub type Reels = Arc<dyn Fn(&Path, &[u8; 32]) -> io::Result<Box<dyn Reel>> + Send + Sync>;

/// The real tape port: encrypted minute files on disk via [`Writer::create`].
pub fn reels() -> Reels {
    Arc::new(|dir, key| Ok(Box::new(Writer::create(dir, key)?) as Box<dyn Reel>))
}

/// A tape that is closed is on the disk.
impl Drop for Writer {
    fn drop(&mut self) {
        if self.dirty {
            let _ = self.file.sync_data(); // dropped: Drop cannot return errors; normal close syncs explicitly via Reel::sync
        }
    }
}

/// The minutes on the disk, in order.
fn minutes(dir: &Path) -> io::Result<Vec<u32>> {
    let names = fs::read_dir(dir)?.filter_map(|entry| entry.ok()?.file_name().into_string().ok());
    let mut minutes: Vec<u32> =
        names.filter_map(|n| n.strip_suffix(".tape")?.parse().ok()).collect();
    minutes.sort_unstable();
    Ok(minutes)
}

fn scan(dir: &Path, key: &[u8; 32], repair: bool, mut each: impl FnMut(Block)) -> io::Result<u32> {
    let (cipher, minutes) = (Aes256Gcm::new(key.into()), minutes(dir)?);
    let mut blocks = 0;
    for (i, &minute) in minutes.iter().enumerate() {
        let path = file(dir, minute);
        let bytes = fs::read(&path)?;
        let whole = bytes.len() / SEALED * SEALED;
        if whole < bytes.len() {
            // Only a crash leaves a short tail, and only on the file being written.
            if i + 1 < minutes.len() {
                return Err(bad("a short block inside the recording"));
            }
            if repair {
                OpenOptions::new().write(true).open(&path)?.set_len(whole as u64)?;
            }
        }
        for (index, sealed) in bytes[..whole].chunks_exact(SEALED).enumerate() {
            let plain = cipher.decrypt(&nonce(minute, index as u32).into(), sealed);
            let plain = plain.map_err(|_| bad("block does not open"))?;
            let pcm = plain.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
            each(Block { t_ms: minute * 60_000 + index as u32 * 100, pcm });
            blocks += 1;
        }
    }
    Ok(blocks * 100)
}

/// After a crash: cuts the torn tail off, and returns how much audio is intact, in ms.
/// A missing folder means nothing was written yet (0 ms), matching [`destroy`].
pub fn recover(dir: &Path, key: &[u8; 32]) -> io::Result<u32> {
    match scan(dir, key, true, |_| {}) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(0),
        other => other,
    }
}

/// Every block in order, and the length of the recording in ms. Needs a `Pass`.
pub fn read(dir: &Path, key: &[u8; 32], _pass: &Pass, each: impl FnMut(Block)) -> io::Result<u32> {
    scan(dir, key, false, each)
}

/// Whether any of the audio is on the disk.
pub fn kept(dir: &Path) -> bool {
    minutes(dir).is_ok_and(|minutes| !minutes.is_empty())
}

/// Deletes the audio for good: every minute, then the folder, unless something else is in it
/// (a write's journal that still owes the service a deletion). Gone already is gone.
pub fn destroy(dir: &Path) -> io::Result<()> {
    let minutes = match minutes(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        minutes => minutes?,
    };
    minutes.into_iter().try_for_each(|minute| fs::remove_file(file(dir, minute)))?;
    match fs::remove_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        done => done,
    }
}
