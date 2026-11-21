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

/// Bytes of one sealed block: the samples, plus the authentication tag.
const SEALED: usize = BLOCK * 2 + 16;

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
}

impl Writer {
    /// `key` belongs to this recording alone: positions repeat across recordings.
    pub fn create(dir: &Path, key: &[u8; 32]) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.into(),
            cipher: Aes256Gcm::new(key.into()),
            minute: 0,
            file: open(dir, 0)?,
        })
    }

    /// Blocks must arrive in order and without holes, which is what `Clock` produces.
    pub fn append(&mut self, block: &Block) -> io::Result<()> {
        let (minute, index) = (block.t_ms / 60_000, block.t_ms % 60_000 / 100);
        if minute != self.minute {
            // A finished minute is made durable. The current one rides the page cache:
            // syncing ten times a second is far too slow on macOS.
            self.file.sync_data()?;
            self.file = open(&self.dir, minute)?;
            self.minute = minute;
        }
        let plain: Vec<u8> = block.pcm.iter().flat_map(|s| s.to_le_bytes()).collect();
        let sealed = self.cipher.encrypt(&nonce(minute, index).into(), plain.as_slice());
        self.file.write_all(&sealed.map_err(|_| bad("block does not seal"))?)
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}

/// A tape that is closed is on the disk.
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.file.sync_data();
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
pub fn recover(dir: &Path, key: &[u8; 32]) -> io::Result<u32> {
    scan(dir, key, true, |_| {})
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
