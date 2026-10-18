use fsevent_sys::{core_foundation::*, *};
use serde::{Deserialize, Serialize};
use std::{
    ffi::{CStr, CString, OsString, c_void},
    fs::OpenOptions,
    io,
    os::fd::AsRawFd,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::PathBuf,
    ptr,
    sync::mpsc::Sender,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::index::Message;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct RootIdentity {
    path: Vec<u8>,
    inode: u64,
    volume: [u8; 16],
}

#[derive(Clone, Debug)]
pub(crate) struct Event {
    pub path: PathBuf,
    pub flags: u32,
    pub id: u64,
    pub stream: Option<usize>,
    pub generation: u64,
}

impl Event {
    pub fn history_done(&self) -> bool {
        self.flags & kFSEventStreamEventFlagHistoryDone != 0
    }
    pub fn lost_history(&self) -> bool {
        self.flags
            & (kFSEventStreamEventFlagUserDropped
                | kFSEventStreamEventFlagKernelDropped
                | kFSEventStreamEventFlagEventIdsWrapped
                | kFSEventStreamEventFlagRootChanged
                | kFSEventStreamEventFlagMount
                | kFSEventStreamEventFlagUnmount)
            != 0
    }
    pub fn recursive(&self) -> bool {
        self.flags
            & (kFSEventStreamEventFlagMustScanSubDirs
                | kFSEventStreamEventFlagItemCreated
                | kFSEventStreamEventFlagItemRemoved
                | kFSEventStreamEventFlagItemRenamed)
            != 0
            || self.flags
                & (kFSEventStreamEventFlagItemModified
                    | kFSEventStreamEventFlagItemInodeMetaMod
                    | kFSEventStreamEventFlagItemFinderInfoMod
                    | kFSEventStreamEventFlagItemChangeOwner
                    | kFSEventStreamEventFlagItemXattrMod)
                == 0
    }
}

#[repr(C)]
struct UuidBytes {
    bytes: [u8; 16],
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventsCopyUUIDForDevice(device: libc::dev_t) -> CFRef;
    fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: *mut c_void);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFUUIDGetUUIDBytes(uuid: CFRef) -> UuidBytes;
}
unsafe extern "C" {
    fn dispatch_queue_create(label: *const i8, attr: *const c_void) -> *mut c_void;
    fn dispatch_sync_f(queue: *mut c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    fn dispatch_release(object: *mut c_void);
}

pub(crate) struct Plan {
    pub identities: Vec<RootIdentity>,
    directories: Vec<(libc::dev_t, PathBuf)>,
}

impl Plan {
    pub(crate) fn new(roots: &[PathBuf]) -> Option<Self> {
        // shortcut: trees spanning mounts/firmlinks verify at startup; add per-volume alias coverage for whole-disk replay.
        let mut mounts = Vec::new();
        unsafe {
            // getmntinfo uses shared process storage; workers need their own snapshot.
            let count = libc::getfsstat(ptr::null_mut(), 0, libc::MNT_NOWAIT);
            if count < 0 {
                return None;
            }
            let mut records: Vec<libc::statfs> = vec![std::mem::zeroed(); count as usize + 8];
            let size = std::mem::size_of_val(records.as_slice()).try_into().ok()?;
            let copied = libc::getfsstat(records.as_mut_ptr(), size, libc::MNT_NOWAIT);
            if copied < 0 || copied as usize >= records.len() {
                return None;
            }
            for record in records.iter().take(copied as usize) {
                let mount = CStr::from_ptr(record.f_mntonname.as_ptr());
                mounts.push(PathBuf::from(OsString::from_vec(mount.to_bytes().to_vec())));
            }
        }
        let firmlinks: Vec<_> = std::fs::read_to_string("/usr/share/firmlinks")
            .ok()?
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|line| !line.starts_with('#'))
            .map(|path| PathBuf::from("/").join(path))
            .collect();
        let mut identities = Vec::new();
        let mut directories = Vec::new();
        for path in roots {
            let metadata = std::fs::symlink_metadata(path).ok()?;
            if !metadata.is_dir()
                || firmlinks
                    .iter()
                    .any(|link| link != path && link.starts_with(path))
            {
                return None;
            }
            let directory = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_EVTONLY | libc::O_NOFOLLOW)
                .open(path)
                .ok()?;
            let metadata = directory.metadata().ok()?;
            let mut physical = [0i8; libc::PATH_MAX as usize];
            let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
            let physical = unsafe {
                if libc::fcntl(
                    directory.as_raw_fd(),
                    libc::F_GETPATH_NOFIRMLINK,
                    physical.as_mut_ptr(),
                ) < 0
                    || libc::fstatfs(directory.as_raw_fd(), &mut stat) != 0
                {
                    return None;
                }
                PathBuf::from(OsString::from_vec(
                    CStr::from_ptr(physical.as_ptr()).to_bytes().to_vec(),
                ))
            };
            if mounts.iter().any(|mount| {
                (mount != path && mount.starts_with(path))
                    || (mount != &physical && mount.starts_with(&physical))
            }) {
                return None;
            }
            let mount = unsafe {
                PathBuf::from(OsString::from_vec(
                    CStr::from_ptr(stat.f_mntonname.as_ptr())
                        .to_bytes()
                        .to_vec(),
                ))
            };
            let relative = physical.strip_prefix(mount).ok()?.to_owned();
            let device = metadata.dev() as libc::dev_t;
            let volume = unsafe {
                let uuid = FSEventsCopyUUIDForDevice(device);
                if uuid.is_null() {
                    return None;
                }
                let bytes = CFUUIDGetUUIDBytes(uuid).bytes;
                CFRelease(uuid);
                bytes
            };
            identities.push(RootIdentity {
                path: path.as_os_str().as_bytes().to_vec(),
                inode: metadata.ino(),
                volume,
            });
            directories.push((device, relative));
        }
        Some(Self {
            identities,
            directories,
        })
    }

    pub(crate) fn current_ids(&self) -> Vec<u64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.directories
            .iter()
            .map(|(device, _)| unsafe { FSEventsGetLastEventIdForDeviceBeforeTime(*device, now) })
            .collect()
    }
}

struct Context {
    sender: Sender<Message>,
    stream: Option<usize>,
    generation: u64,
    root: PathBuf,
    relative: PathBuf,
}

pub(crate) fn current_id() -> u64 {
    unsafe { FSEventsGetCurrentEventId() }
}

pub(crate) struct Watcher {
    _streams: Vec<Stream>,
}
struct Stream {
    stream: FSEventStreamRef,
    queue: *mut c_void,
    context: *mut Context,
    started: bool,
}

impl Watcher {
    pub(crate) fn start(
        roots: &[PathBuf],
        plan: Option<&Plan>,
        since: &[u64],
        generation: u64,
        sender: Sender<Message>,
    ) -> io::Result<Self> {
        let mut streams = Vec::new();
        if let Some(plan) = plan {
            for (i, (device, relative)) in plan.directories.iter().enumerate() {
                streams.push(Stream::start(
                    std::slice::from_ref(relative),
                    Some(*device),
                    since[i],
                    Context {
                        sender: sender.clone(),
                        stream: Some(i),
                        generation,
                        root: roots[i].clone(),
                        relative: relative.clone(),
                    },
                )?);
            }
        } else {
            streams.push(Stream::start(
                roots,
                None,
                kFSEventStreamEventIdSinceNow,
                Context {
                    sender,
                    stream: None,
                    generation,
                    root: PathBuf::new(),
                    relative: PathBuf::new(),
                },
            )?);
        }
        Ok(Self { _streams: streams })
    }
}

impl Stream {
    fn start(
        roots: &[PathBuf],
        device: Option<libc::dev_t>,
        since: u64,
        context: Context,
    ) -> io::Result<Self> {
        unsafe {
            let paths = CFArrayCreateMutable(ptr::null_mut(), 0, &kCFTypeArrayCallBacks);
            if paths.is_null() {
                return Err(io::Error::other("Cannot allocate watched paths"));
            }
            for root in roots {
                let path = match CString::new(root.as_os_str().as_bytes()) {
                    Ok(path) => CFStringCreateWithCString(
                        ptr::null_mut(),
                        path.as_ptr(),
                        kCFStringEncodingUTF8,
                    ),
                    Err(_) => ptr::null_mut(),
                };
                if path.is_null() {
                    CFRelease(paths);
                    return Err(io::Error::other("Cannot represent watched folder"));
                }
                CFArrayAppendValue(paths, path);
                CFRelease(path);
            }
            let context = Box::into_raw(Box::new(context));
            let info = FSEventStreamContext {
                version: 0,
                info: context.cast(),
                retain: None,
                release: None,
                copy_description: None,
            };
            let flags = kFSEventStreamCreateFlagFileEvents
                | kFSEventStreamCreateFlagWatchRoot
                | kFSEventStreamCreateFlagNoDefer
                | kFSEventStreamCreateFlagFullHistory;
            let stream = if let Some(device) = device {
                FSEventStreamCreateRelativeToDevice(
                    ptr::null_mut(),
                    callback,
                    &info,
                    device,
                    paths,
                    since,
                    0.2,
                    flags,
                )
            } else {
                FSEventStreamCreate(ptr::null_mut(), callback, &info, paths, since, 0.2, flags)
            };
            CFRelease(paths);
            if stream.is_null() {
                drop(Box::from_raw(context));
                return Err(io::Error::other("Cannot create filesystem event stream"));
            }
            let queue = dispatch_queue_create(c"EverythingMac.index".as_ptr(), ptr::null());
            if queue.is_null() {
                FSEventStreamRelease(stream);
                drop(Box::from_raw(context));
                return Err(io::Error::other("Cannot create filesystem event queue"));
            }
            let mut watcher = Self {
                stream,
                queue,
                context,
                started: false,
            };
            FSEventStreamSetDispatchQueue(stream, queue);
            if FSEventStreamStart(stream) == 0 {
                return Err(io::Error::other("Cannot start filesystem event stream"));
            }
            watcher.started = true;
            Ok(watcher)
        }
    }
}

impl Context {
    fn map_event(&self, mut event: Event) -> Option<Event> {
        if self.stream.is_some() {
            if event.history_done() || event.lost_history() {
                event.path = self.root.clone();
            } else {
                let relative = event.path.strip_prefix("/").unwrap_or(&event.path);
                if let Ok(suffix) = relative.strip_prefix(&self.relative) {
                    event.path = self.root.join(suffix);
                } else if event.recursive() && self.relative.starts_with(relative) {
                    event.path = self.root.clone();
                } else {
                    return None;
                }
            }
        }
        Some(event)
    }
}

extern "C" fn callback(
    _: FSEventStreamRef,
    context: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
) {
    // The stream owns these arrays only for the duration of this callback.
    unsafe {
        let context = &*context.cast::<Context>();
        let paths = paths.cast::<*const i8>();
        let events = (0..count)
            .filter_map(|i| {
                let event = Event {
                    path: PathBuf::from(OsString::from_vec(
                        CStr::from_ptr(*paths.add(i)).to_bytes().to_vec(),
                    )),
                    flags: *flags.add(i),
                    id: *ids.add(i),
                    stream: context.stream,
                    generation: context.generation,
                };
                context.map_event(event)
            })
            .collect();
        let _ = context.sender.send(Message::Events(events));
    }
}

extern "C" fn drained(_: *mut c_void) {}
impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            if self.started {
                FSEventStreamStop(self.stream);
            }
            FSEventStreamInvalidate(self.stream);
            // Wait for callbacks before freeing their channel context.
            dispatch_sync_f(self.queue, ptr::null_mut(), drained);
            FSEventStreamRelease(self.stream);
            drop(Box::from_raw(self.context));
            dispatch_release(self.queue);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_device_paths_back_to_configured_paths_and_keeps_coalesced_ancestors() {
        let (sender, _) = std::sync::mpsc::channel();
        let context = Context {
            sender,
            stream: Some(0),
            generation: 0,
            root: PathBuf::from("/Users/test"),
            relative: PathBuf::from("Users/test"),
        };
        let event = |path: &str, flags| Event {
            path: PathBuf::from(path),
            flags,
            id: 5,
            stream: Some(0),
            generation: 0,
        };
        assert_eq!(
            context
                .map_event(event("Users/test/a", kFSEventStreamEventFlagItemCreated))
                .unwrap()
                .path,
            PathBuf::from("/Users/test/a")
        );
        assert_eq!(
            context
                .map_event(event("/Users/test/a", kFSEventStreamEventFlagItemModified))
                .unwrap()
                .path,
            PathBuf::from("/Users/test/a")
        );
        assert_eq!(
            context
                .map_event(event("Users", kFSEventStreamEventFlagMustScanSubDirs))
                .unwrap()
                .path,
            context.root
        );
        assert_eq!(
            context
                .map_event(event(
                    "ignored sentinel",
                    kFSEventStreamEventFlagHistoryDone
                ))
                .unwrap()
                .path,
            context.root
        );
        assert!(
            context
                .map_event(event(
                    "Users/another/a",
                    kFSEventStreamEventFlagItemModified
                ))
                .is_none()
        );
        assert!(
            Plan::new(&[PathBuf::from("/")]).is_none(),
            "Whole-disk trees cross mount/firmlink boundaries and must use full verification"
        );
    }

    #[test]
    fn classifies_recovery_history_and_directory_diffs() {
        let event = |flags| Event {
            path: PathBuf::from("/"),
            flags,
            id: 1,
            stream: Some(0),
            generation: 0,
        };
        assert!(event(kFSEventStreamEventFlagHistoryDone).history_done());
        for flag in [
            kFSEventStreamEventFlagUserDropped,
            kFSEventStreamEventFlagKernelDropped,
            kFSEventStreamEventFlagEventIdsWrapped,
            kFSEventStreamEventFlagRootChanged,
            kFSEventStreamEventFlagMount,
            kFSEventStreamEventFlagUnmount,
        ] {
            assert!(event(flag).lost_history());
        }
        assert!(event(kFSEventStreamEventFlagMustScanSubDirs).recursive());
        assert!(event(kFSEventStreamEventFlagItemRenamed).recursive());
        assert!(!event(kFSEventStreamEventFlagItemModified).recursive());
    }
}
