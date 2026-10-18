use crate::index::Entry;
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStringExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, UNIX_EPOCH},
};

const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | ATTR_CMN_ERROR
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME;
const FILE_ATTRIBUTES: u32 = libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

pub(crate) fn metadata(path: &Path) -> io::Result<Entry> {
    let metadata = fs::symlink_metadata(path)?;
    Ok(Entry {
        path: path.to_owned(),
        is_dir: metadata.is_dir(),
        size: metadata.len(),
        allocated_size: metadata.blocks() * 512,
        modified: metadata.modified().ok(),
    })
}

pub(crate) fn read_directory(
    path: &Path,
    stopped: &AtomicBool,
) -> io::Result<Vec<(PathBuf, io::Result<Entry>)>> {
    // Some filesystems cannot supply bulk attributes. Retry the whole listing normally.
    bulk(path, stopped).or_else(|error| {
        if stopped.load(Ordering::Relaxed) {
            return Err(error);
        }
        if !fs::symlink_metadata(path)?.is_dir() {
            return Err(io::Error::other("Directory was replaced"));
        }
        let mut entries = Vec::new();
        for item in fs::read_dir(path)? {
            if stopped.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let item = item?;
            let path = item.path();
            entries.push((path.clone(), metadata(&path)));
        }
        Ok(entries)
    })
}

fn bulk(path: &Path, stopped: &AtomicBool) -> io::Result<Vec<(PathBuf, io::Result<Entry>)>> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut attributes: libc::attrlist = unsafe { std::mem::zeroed() };
    attributes.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attributes.commonattr = COMMON;
    attributes.fileattr = FILE_ATTRIBUTES;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut entries = Vec::new();
    loop {
        if stopped.load(Ordering::Relaxed) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let count = unsafe {
            libc::getattrlistbulk(
                directory.as_raw_fd(),
                (&raw mut attributes).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::FSOPT_PACK_INVAL_ATTRS as _,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Ok(entries);
        }
        let mut offset = 0;
        for _ in 0..count {
            if stopped.load(Ordering::Relaxed) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let length = u32_at(&buffer, offset)? as usize;
            if length < 56 {
                return Err(invalid());
            }
            let end = offset.checked_add(length).ok_or_else(invalid)?;
            let record = buffer.get(offset..end).ok_or_else(invalid)?;
            let (name, entry) = decode(path, record)?;
            // Directories, links, and missing/failed attributes need lstat semantics.
            let path = path.join(name);
            entries.push((
                path.clone(),
                match entry {
                    Some(entry) => Ok(entry),
                    None => metadata(&path),
                },
            ));
            offset = end;
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid bulk directory attributes",
    )
}
fn u32_at(b: &[u8], at: usize) -> io::Result<u32> {
    Ok(u32::from_ne_bytes(
        b.get(at..at.checked_add(4).ok_or_else(invalid)?)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}
fn u64_at(b: &[u8], at: usize) -> io::Result<u64> {
    Ok(u64::from_ne_bytes(
        b.get(at..at.checked_add(8).ok_or_else(invalid)?)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}

fn decode(parent: &Path, b: &[u8]) -> io::Result<(OsString, Option<Entry>)> {
    // FSOPT_PACK_INVAL_ATTRS keeps the requested slots even when a field is unavailable.
    let common = u32_at(b, 4)?;
    if common & libc::ATTR_CMN_NAME == 0 {
        return Err(invalid());
    }
    let start = 28i64
        .checked_add(u32_at(b, 28)? as i32 as i64)
        .ok_or_else(invalid)?;
    let start = usize::try_from(start).map_err(|_| invalid())?;
    let len = u32_at(b, 32)? as usize;
    if start < 56 || len < 2 {
        return Err(invalid());
    }
    let name = b
        .get(start..start.checked_add(len).ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    if name.last() != Some(&0) {
        return Err(invalid());
    }
    let name = &name[..name.len() - 1];
    if name.contains(&0) || name.contains(&b'/') || name == b"." || name == b".." {
        return Err(invalid());
    }
    let name = OsString::from_vec(name.to_vec());
    let required = libc::ATTR_CMN_NAME | libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_MODTIME;
    if common & required != required
        || u32_at(b, 24)? != 0
        || u32_at(b, 36)? != 1
        || u32_at(b, 16)? & FILE_ATTRIBUTES != FILE_ATTRIBUTES
    {
        return Ok((name, None));
    }
    if start < 72 {
        return Err(invalid());
    }
    let seconds = u64_at(b, 40)? as i64;
    let nanos = u64_at(b, 48)? as i64;
    if !(0..1_000_000_000).contains(&nanos) {
        return Err(invalid());
    }
    let modified = if seconds >= 0 {
        UNIX_EPOCH.checked_add(Duration::new(seconds as u64, nanos as u32))
    } else {
        UNIX_EPOCH
            .checked_sub(Duration::from_secs(seconds.unsigned_abs()))
            .and_then(|time| time.checked_add(Duration::from_nanos(nanos as u64)))
    }
    .ok_or_else(invalid)?;
    let entry = Entry {
        path: parent.join(&name),
        is_dir: false,
        allocated_size: u64_at(b, 56)?,
        size: u64_at(b, 64)?,
        modified: Some(modified),
    };
    Ok((name, Some(entry)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::{ffi::OsStrExt, fs::symlink},
        time::SystemTime,
    };

    #[test]
    fn native_bulk_matches_lstat_for_files_directories_links_and_raw_names() {
        let root = std::env::temp_dir().join(format!(
            "everything-bulk-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("regular"), b"hello").unwrap();
        let sparse = fs::File::create(root.join("sparse.raw")).unwrap();
        sparse.set_len(1024 * 1024 * 1024).unwrap();
        fs::write(root.join("İ résumé 🐈"), b"unicode").unwrap();
        fs::create_dir(root.join("folder")).unwrap();
        symlink("folder", root.join("link")).unwrap();
        symlink("missing", root.join("broken")).unwrap();
        let file = fs::File::open(root.join("regular")).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH - Duration::new(10, 123)))
            .unwrap();
        let entries = bulk(&root, &AtomicBool::new(false))
            .expect("Native bulk path must work, not silently fall back");
        assert_eq!(entries.len(), 6);
        for (path, entry) in entries {
            let entry = entry.unwrap();
            assert_eq!(entry, metadata(&path).unwrap());
            if path.ends_with("sparse.raw") {
                assert_eq!(entry.size, 1024 * 1024 * 1024);
                assert!(entry.allocated_size < entry.size);
            }
        }
        assert!(
            bulk(&root.join("link"), &AtomicBool::new(false)).is_err(),
            "Do not traverse a root symlink"
        );
        let stopped = AtomicBool::new(true);
        assert_eq!(
            read_directory(&root, &stopped).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn record(name: &[u8]) -> Vec<u8> {
        let mut b = vec![0; 72 + name.len()];
        let len = b.len() as u32;
        b[..4].copy_from_slice(&len.to_ne_bytes());
        b[4..8].copy_from_slice(&COMMON.to_ne_bytes());
        b[16..20].copy_from_slice(&FILE_ATTRIBUTES.to_ne_bytes());
        b[28..32].copy_from_slice(&44i32.to_ne_bytes());
        b[32..36].copy_from_slice(&(name.len() as u32).to_ne_bytes());
        b[36..40].copy_from_slice(&1u32.to_ne_bytes());
        b[56..64].copy_from_slice(&4096u64.to_ne_bytes());
        b[64..72].copy_from_slice(&123u64.to_ne_bytes());
        b[72..].copy_from_slice(name);
        b
    }

    #[test]
    fn rejects_malformed_native_records_and_falls_back_for_missing_attributes() {
        let parent = Path::new("/test");
        let b = record(b"ok\0");
        let (raw, _) = decode(parent, &record(b"raw-\xff\0")).unwrap();
        assert_eq!(raw.as_bytes(), b"raw-\xff");
        assert_eq!(decode(parent, &b).unwrap().1.unwrap().size, 123);
        assert_eq!(decode(parent, &b).unwrap().1.unwrap().allocated_size, 4096);
        for end in 0..b.len() {
            assert!(decode(parent, &b[..end]).is_err());
        }
        for name in [
            b"../bad\0".as_slice(),
            b"..\0",
            b".\0",
            b"nul\0x\0",
            b"bad",
            b"\0",
        ] {
            assert!(decode(parent, &record(name)).is_err());
        }
        let mut invalid_offset = b.clone();
        invalid_offset[28..32].copy_from_slice(&i32::MIN.to_ne_bytes());
        assert!(decode(parent, &invalid_offset).is_err());
        let mut nanos = b.clone();
        nanos[48..56].copy_from_slice(&1_000_000_000u64.to_ne_bytes());
        assert!(decode(parent, &nanos).is_err());
        let mut missing = b;
        missing[16..20].fill(0);
        assert!(decode(parent, &missing).unwrap().1.is_none());
    }

    #[test]
    #[ignore = "local enumeration timing; creates 4000 temporary files"]
    fn enumeration_timing() {
        let root =
            std::env::temp_dir().join(format!("everything-bulk-timing-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        for i in 0..4000 {
            fs::write(root.join(format!("file-{i}")), b"data").unwrap();
        }
        let started = std::time::Instant::now();
        let normal: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|item| metadata(&item.unwrap().path()).unwrap())
            .collect();
        let normal_time = started.elapsed();
        let started = std::time::Instant::now();
        let entries = bulk(&root, &AtomicBool::new(false)).unwrap();
        let bulk_time = started.elapsed();
        assert_eq!(normal.len(), entries.len());
        assert!(entries.iter().all(|(_, entry)| entry.is_ok()));
        eprintln!("4000 files: read_dir+lstat {normal_time:?}; getattrlistbulk {bulk_time:?}");
        fs::remove_dir_all(root).unwrap();
    }
}
