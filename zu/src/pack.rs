//! The only IO of the kernel: a pack, an append-only file of records and the bodies they
//! link to, one header or one body per line as JSON text, so that any tool can read and
//! search it.
//!
//! Crash safety: a crash can leave a torn last line. [`Pack::open`] cuts it off, so that a new
//! record never lands behind the remains of an old one. A line that is not a record and is
//! not the last is damage: it is reported as [`Error::Corrupt`], and the file is left as it is.
//!
//! Durability: [`Pack::append`] does not wait for the disk, so a line survives the process but
//! not a loss of power. [`Pack::sync`] waits. On macOS that is `F_FULLFSYNC`, milliseconds
//! each time, so it is the caller who decides when, and it is not done for every record.
//!
//! Failure: after a failed write or sync the state of the file is unknown. The pack answers
//! every later call with the same error; open it again, which checks the file anew.

use crate::{Cid, Codec, Error, Record, Value};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// What a pack holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Held {
    pub records: Vec<Record>,
    /// Bodies by their CID. A record is a record without its body (record.md § Header).
    pub bodies: BTreeMap<Cid, Value>,
}

/// A pack open for appending. The file is locked while it is open: one writer at a time.
#[derive(Debug)]
pub struct Pack {
    file: File,
    failed: Option<ErrorKind>,
}

impl Pack {
    /// Open the pack at `path`, or create it, and return it with what it holds.
    pub fn open(path: impl AsRef<Path>) -> Result<(Pack, Held), Error> {
        let (path, mut bytes) = (path.as_ref(), Vec::new());
        let mut file =
            File::options().read(true).write(true).create(true).truncate(false).open(path)?;
        file.try_lock().map_err(std::io::Error::from)?;
        file.read_to_end(&mut bytes)?;
        let (held, intact) = scan(&bytes)?;
        if bytes.is_empty() && cfg!(unix) {
            // A new file is durable once it and its directory are.
            let dir = path.parent().filter(|dir| *dir != Path::new("")).unwrap_or(Path::new("."));
            file.sync_all().and_then(|()| File::open(dir)?.sync_all())?;
        }
        file.set_len(intact as u64)?;
        file.seek(SeekFrom::Start(intact as u64))?;
        Ok((Pack { file, failed: None }, held))
    }

    /// What the pack at `path` holds, without changing it. A torn last line is left out.
    /// Each header is checked for its structure; its signature is for [`Record::verify`].
    pub fn read(path: impl AsRef<Path>) -> Result<Held, Error> {
        Ok(scan(&std::fs::read(path)?)?.0)
    }

    /// Append one record as one line, unless it would not read back the same.
    pub fn append(&mut self, record: &Record) -> Result<(), Error> {
        let line = record.header().to_json()? + "\n";
        self.io(|file| file.write_all(line.as_bytes()))
    }

    /// Append a body as one line, and return the CID a record links to it by.
    pub fn keep(&mut self, body: &Value) -> Result<Cid, Error> {
        crate::rule(header(body) == Some(false), Error::Field("body"))?;
        let line = body.to_json()? + "\n";
        self.io(|file| file.write_all(line.as_bytes()))?;
        Ok(Cid::of(Codec::Drisl, &body.encode()))
    }

    /// Wait until everything appended so far is on the disk.
    pub fn sync(&mut self) -> Result<(), Error> {
        self.io(|file| file.sync_data())
    }

    fn io(&mut self, op: impl FnOnce(&mut File) -> std::io::Result<()>) -> Result<(), Error> {
        if self.failed.is_none() {
            self.failed = op(&mut self.file).err().map(|error| error.kind());
        }
        self.failed.map_or(Ok(()), |kind| Err(Error::Io(kind)))
    }
}

/// What a line is: a record's header (`true`), or a body, which record.md § Body says is
/// a map (`false`). Anything else is not a line of a pack.
fn header(value: &Value) -> Option<bool> {
    let Value::Map(map) = value else { return None };
    Some(map.get("$type") == Some(&Value::Str("record".into())))
}

/// What the intact lines hold, and the length in bytes of those lines.
fn scan(bytes: &[u8]) -> Result<(Held, usize), Error> {
    let (mut held, mut intact) = (Held::default(), 0);
    let mut lines = bytes.split_inclusive(|&byte| byte == b'\n').enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        let json = line.strip_suffix(b"\n").and_then(|json| std::str::from_utf8(json).ok());
        let kept = json.ok_or(Error::Json).and_then(Value::from_json).and_then(|value| {
            match header(&value) {
                Some(true) => held.records.push(Record::try_from(value)?),
                Some(false) => {
                    _ = held.bodies.insert(Cid::of(Codec::Drisl, &value.encode()), value)
                }
                None => return Err(Error::Json),
            }
            Ok(())
        });
        match kept {
            Ok(()) => intact += line.len(),
            // The tail: a line the crash cut short, or left unreadable.
            Err(_) if lines.peek().is_none() => break,
            Err(_) => return Err(Error::Corrupt(index + 1)),
        }
    }
    Ok((held, intact))
}
