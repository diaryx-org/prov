//! A minimal, store-only (uncompressed) ZIP writer for `prov backup --zip`.
//!
//! prov keeps its dependency surface tiny and WASM-clean (no build-toolchain
//! cost, nothing to audit) — the same reason [`prov::exec::block_on`] is fifteen
//! lines rather than an async runtime. A *store-only* ZIP archive (method
//! 0: bytes copied verbatim, no DEFLATE) is small enough to hand-write the same
//! way: local file headers, a central directory, and an end-of-central-directory
//! record, per the format's own spec (PKWARE APPNOTE.TXT §4.3). CRC-32 is the
//! one algorithm the format needs beyond bookkeeping, so it is hand-rolled here
//! too, table-driven, and checked below against the standard `"123456789"`
//! check value.
//!
//! What is deliberately absent: compression (store-only — a backup archive
//! trades size for zero new code), ZIP64 (a >4 GiB archive or one with more
//! than 65 535 entries is out of scope for a single workspace backup and
//! [`ZipWriter::finish`] refuses cleanly rather than silently truncating an
//! offset), and any per-platform extra field (so no symlink support — see
//! `backup::add_dir_to_zip`, which skips symlinks with a warning instead).

use std::io::{self, Write};

/// The CRC-32 table (reflected, polynomial 0xEDB88320 — the same "CRC-32/
/// ISO-HDLC" variant `zlib`/`gzip`/ZIP itself uses), built once at compile time.
const CRC32_TABLE: [u32; 256] = build_crc32_table();

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0usize;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

/// The CRC-32 (ISO-HDLC) checksum of `bytes`, as ZIP's local/central headers
/// record it.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        let idx = ((crc ^ b as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[idx] ^ (crc >> 8);
    }
    !crc
}

/// A ZIP entry's timestamp, in the format's native MS-DOS date/time fields —
/// callers build this with [`crate::backup::dos_datetime`], never a clock read
/// here (this module has none).
pub(crate) type DosTime = (u16, u16);

/// One central-directory record, accumulated as entries are written so
/// [`ZipWriter::finish`] can emit them together at the end (the format's own
/// layout: all local entries, then the whole central directory, then the
/// end-of-central-directory record).
struct CentralRecord {
    name: String,
    crc32: u32,
    size: u32,
    dos_time: u16,
    dos_date: u16,
    /// MS-DOS-host external attributes: the directory bit, or none.
    external_attrs: u32,
    local_header_offset: u32,
}

/// A store-only ZIP writer over any [`Write`] sink (a [`std::fs::File`], or
/// (in tests) an in-memory `Vec<u8>`). Entries must be added in the order they
/// should appear in the archive; [`finish`] closes it out.
///
/// [`finish`]: ZipWriter::finish
pub(crate) struct ZipWriter<W: Write> {
    out: W,
    offset: u64,
    central: Vec<CentralRecord>,
}

/// Version 2.0 — the lowest that a "store" (no compression) entry needs
/// (PKWARE APPNOTE.TXT §4.4.3).
const VERSION_NEEDED: u16 = 20;
/// General-purpose bit 11 (0x0800): entry and comment names are UTF-8, so a
/// title with non-ASCII characters round-trips through any modern unzip
/// (Archive Utility, Python's `zipfile`, `unzip`) rather than being
/// reinterpreted as CP437.
const UTF8_NAME_FLAG: u16 = 0x0800;
/// MS-DOS external attribute bit for a directory entry.
const ATTR_DIRECTORY: u32 = 0x10;
/// MS-DOS external attribute bit for a plain file entry (the "archive" bit;
/// conventional, not load-bearing).
const ATTR_ARCHIVE: u32 = 0x20;

impl<W: Write> ZipWriter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            offset: 0,
            central: Vec::new(),
        }
    }

    /// Append a directory entry. `name` must end in `/` (the convention that
    /// tells an unzip this is a directory rather than a zero-byte file).
    pub(crate) fn add_dir(&mut self, name: &str, at: DosTime) -> io::Result<()> {
        debug_assert!(name.ends_with('/'), "zip directory entries must end in /");
        self.write_entry(name, &[], 0, at, ATTR_DIRECTORY)
    }

    /// Append a file entry with its literal (uncompressed) bytes.
    pub(crate) fn add_file(&mut self, name: &str, data: &[u8], at: DosTime) -> io::Result<()> {
        let crc = crc32(data);
        self.write_entry(name, data, crc, at, ATTR_ARCHIVE)
    }

    fn write_entry(
        &mut self,
        name: &str,
        data: &[u8],
        crc: u32,
        (dos_time, dos_date): DosTime,
        external_attrs: u32,
    ) -> io::Result<()> {
        let name_bytes = name.as_bytes();
        let size: u32 = data
            .len()
            .try_into()
            .map_err(|_| io::Error::other(format!("entry {name} exceeds 4 GiB (no ZIP64)")))?;
        let local_header_offset: u32 = self
            .offset
            .try_into()
            .map_err(|_| io::Error::other("archive exceeds 4 GiB (no ZIP64)"))?;

        let mut header = Vec::with_capacity(30 + name_bytes.len());
        header.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header signature
        header.extend_from_slice(&VERSION_NEEDED.to_le_bytes());
        header.extend_from_slice(&UTF8_NAME_FLAG.to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // compression method: store
        header.extend_from_slice(&dos_time.to_le_bytes());
        header.extend_from_slice(&dos_date.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes()); // compressed size == uncompressed (store)
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        header.extend_from_slice(name_bytes);

        self.out.write_all(&header)?;
        self.out.write_all(data)?;
        self.offset += header.len() as u64 + data.len() as u64;

        self.central.push(CentralRecord {
            name: name.to_string(),
            crc32: crc,
            size,
            dos_time,
            dos_date,
            external_attrs,
            local_header_offset,
        });
        Ok(())
    }

    /// Write the central directory and the end-of-central-directory record,
    /// then return the underlying sink (flushed).
    pub(crate) fn finish(mut self) -> io::Result<W> {
        let central_start = self.offset;
        let mut central_bytes = Vec::new();
        for rec in &self.central {
            let name_bytes = rec.name.as_bytes();
            central_bytes.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central dir signature
            central_bytes.extend_from_slice(&VERSION_NEEDED.to_le_bytes()); // version made by
            central_bytes.extend_from_slice(&VERSION_NEEDED.to_le_bytes()); // version needed
            central_bytes.extend_from_slice(&UTF8_NAME_FLAG.to_le_bytes());
            central_bytes.extend_from_slice(&0u16.to_le_bytes()); // method: store
            central_bytes.extend_from_slice(&rec.dos_time.to_le_bytes());
            central_bytes.extend_from_slice(&rec.dos_date.to_le_bytes());
            central_bytes.extend_from_slice(&rec.crc32.to_le_bytes());
            central_bytes.extend_from_slice(&rec.size.to_le_bytes());
            central_bytes.extend_from_slice(&rec.size.to_le_bytes());
            central_bytes.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
            central_bytes.extend_from_slice(&0u16.to_le_bytes()); // extra field length
            central_bytes.extend_from_slice(&0u16.to_le_bytes()); // comment length
            central_bytes.extend_from_slice(&0u16.to_le_bytes()); // disk number start
            central_bytes.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            central_bytes.extend_from_slice(&rec.external_attrs.to_le_bytes());
            central_bytes.extend_from_slice(&rec.local_header_offset.to_le_bytes());
            central_bytes.extend_from_slice(name_bytes);
        }
        self.out.write_all(&central_bytes)?;

        let count: u16 = self
            .central
            .len()
            .try_into()
            .map_err(|_| io::Error::other("more than 65535 entries (no ZIP64)"))?;
        let central_size: u32 = central_bytes
            .len()
            .try_into()
            .map_err(|_| io::Error::other("central directory exceeds 4 GiB (no ZIP64)"))?;
        let central_offset: u32 = central_start
            .try_into()
            .map_err(|_| io::Error::other("archive exceeds 4 GiB (no ZIP64)"))?;

        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // EOCD signature
        eocd.extend_from_slice(&0u16.to_le_bytes()); // this disk
        eocd.extend_from_slice(&0u16.to_le_bytes()); // disk with central dir start
        eocd.extend_from_slice(&count.to_le_bytes()); // entries on this disk
        eocd.extend_from_slice(&count.to_le_bytes()); // entries total
        eocd.extend_from_slice(&central_size.to_le_bytes());
        eocd.extend_from_slice(&central_offset.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes()); // comment length

        self.out.write_all(&eocd)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // The canonical CRC-32/ISO-HDLC check value for the ASCII digits
        // "123456789" — the same one `zlib`, `png`, and every ZIP tool agree on.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// Writes a couple of entries to an in-memory buffer and parses the result
    /// back out by hand (local headers + EOCD), so the writer is checked
    /// structurally without shelling out to `unzip`.
    #[test]
    fn round_trips_through_hand_parsing() {
        let mut zw = ZipWriter::new(Vec::new());
        zw.add_dir("root/", (0, 0x21)).unwrap();
        zw.add_file("root/a.txt", b"hello", (0, 0x21)).unwrap();
        zw.add_file("root/sub/b.txt", b"world!!", (0, 0x21))
            .unwrap();
        let bytes = zw.finish().unwrap();

        // Find the EOCD by its signature (fixed-size, no comment used here, so
        // it is the last 22 bytes).
        let eocd = &bytes[bytes.len() - 22..];
        assert_eq!(&eocd[0..4], &0x0605_4b50u32.to_le_bytes());
        let count = u16::from_le_bytes([eocd[10], eocd[11]]);
        assert_eq!(count, 3);
        let central_offset = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;

        // Walk the central directory and collect names + declared sizes.
        let mut pos = central_offset;
        let mut names = Vec::new();
        for _ in 0..count {
            assert_eq!(&bytes[pos..pos + 4], &0x0201_4b50u32.to_le_bytes());
            let method = u16::from_le_bytes([bytes[pos + 10], bytes[pos + 11]]);
            assert_eq!(method, 0, "store-only");
            let size = u32::from_le_bytes([
                bytes[pos + 24],
                bytes[pos + 25],
                bytes[pos + 26],
                bytes[pos + 27],
            ]);
            let name_len = u16::from_le_bytes([bytes[pos + 28], bytes[pos + 29]]) as usize;
            let name_start = pos + 46;
            let name =
                String::from_utf8(bytes[name_start..name_start + name_len].to_vec()).unwrap();
            names.push((name, size));
            pos = name_start + name_len;
        }
        assert_eq!(
            names,
            vec![
                ("root/".to_string(), 0),
                ("root/a.txt".to_string(), 5),
                ("root/sub/b.txt".to_string(), 7),
            ]
        );
    }
}
