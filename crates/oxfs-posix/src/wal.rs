use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use parking_lot::Mutex;

pub struct Wal {
    path: PathBuf,
    file: Mutex<std::fs::File>,
}

// WAL entry format: [slice_id: u64 LE][data_len: u32 LE][data: bytes]
const ENTRY_HEADER: usize = 8 + 4; // slice_id + data_len

impl Wal {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            file: Mutex::new(file),
        })
    }

    pub fn append(&self, slice_id: u64, data: &[u8]) -> std::io::Result<()> {
        let mut file = self.file.lock();
        file.write_all(&slice_id.to_le_bytes())?;
        file.write_all(&(data.len() as u32).to_le_bytes())?;
        file.write_all(data)?;
        file.flush()?;
        Ok(())
    }

    pub fn read_all(&self) -> std::io::Result<Vec<(u64, Bytes)>> {
        let mut file = self.file.lock();
        let metadata = file.metadata()?;
        if metadata.len() == 0 {
            return Ok(vec![]);
        }

        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0))?;

        let mut entries = Vec::new();
        let mut header = [0u8; ENTRY_HEADER];

        loop {
            match file.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            let slice_id = u64::from_le_bytes(header[0..8].try_into().unwrap());
            let data_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;

            let mut data = vec![0u8; data_len];
            match file.read_exact(&mut data) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }

            entries.push((slice_id, Bytes::from(data)));
        }

        Ok(entries)
    }

    pub fn clear(&self) -> std::io::Result<()> {
        let mut file = self.file.lock();
        file.set_len(0)?;
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(0))?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
