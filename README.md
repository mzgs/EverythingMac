# Everything Mac

A macOS filename-search app built in Rust with the project's existing GPUI Kit interface. It recreates Everything's core search workflow using a background directory index and native macOS filesystem notifications.

## Run

```sh
./run.sh
```

For UI development without macOS folder-access prompts, use `./run.sh --preview`.
This creates sample files and separate settings/index data in a new temporary directory.
Preview never loads your saved indexed folders or window state; the title bar marks it **Preview**.

Requires macOS, the stable Rust toolchain, and Xcode command-line tools. The first Cargo build downloads the locked dependencies. Build an ad-hoc signed app bundle with:

```sh
./build.sh
open "target/release/Everything Mac.app"
```

The bundle is built for the current Mac's architecture. It is not Developer ID signed or notarized for distribution.

GPUI Fast's scroll-tile cache is disabled at startup because version 0.1.6 can overflow its tile coordinates in large/HiDPI result lists. Rows remain virtualized. The separate Cargo future-compatibility warning for `block 0.1.6` is an upstream dependency warning, not this rendering panic.

## Search

Your home folder is indexed by default. Open **Indexed folders** (`⌘,`) to add/remove folders or mounted volumes, then save. **Rebuild cache** is also available in this settings dialog to rescan the saved indexed folders. Nested roots are deduplicated by the indexer. Hidden files are included; symbolic links are listed without following them. Names, sizes and modified times are scanned in the background; file contents are never read by the indexer.

Cloud folders are excluded from scans, saved results, and live updates: macOS `~/Library/CloudStorage` (Google Drive, Dropbox, OneDrive, Box, and other File Provider services), iCloud’s `~/Library/Mobile Documents`, and standard home-folder locations for Dropbox, Google Drive, OneDrive, Box Sync, pCloud Drive, and MEGA. Custom sync locations outside these paths are not detected. Full recursive scans use one directory worker per available logical CPU.

Search updates as you type. Results initially use the index’s existing path order without sorting at launch. Results are virtualized and can be sorted by name, path, size, or modified time. Click a header again to reverse its sort. Filter by files, folders, audio, images, videos, or documents. Right-click a result for **Open**, **Show in Finder**, **Get Info**, **Copy**, **Copy path**, **Rename…**, and **Move to Trash**. **Get Info** opens Finder’s information window and may request permission to control Finder. **Copy** puts the actual file or folder on the clipboard for pasting in Finder. **Rename…** edits its name in place and refuses to replace an existing item. The trash action uses macOS Trash so files and folders can be restored in Finder; failures appear in the search error area. Selecting a result moves keyboard focus from search to the file list. Press Space there to preview the selected result in the app’s native macOS Quick Look panel; Space or Escape closes the preview. Spaces still type normally in the search field. Double-click or press Return to open the selected result with its default app; `⌘Return` reveals it in Finder and `⇧⌘C` copies its full path. `⌘F` focuses search, `↑`/`↓` select results, and `⌘R` rebuilds the index.

| Query | Meaning |
| --- | --- |
| `report budget` | Both words in the filename |
| `"annual report"` | An exact phrase |
| `*.pdf \| *.docx` | Either wildcard pattern |
| `report !backup` | Exclude matching backup names |
| `<cat \| dog> photo` | Group alternatives |
| `ext:jpg;png` | Match extensions |
| `file:` / `folder:` | Match kind |
| `path:Projects` | Match the full path |
| `name:README` | Match the filename |
| `size:>10mb` | Files larger than 10 MiB |
| `size:1kb..10kb` | Inclusive size range |

The Size column and Size sorting use allocated bytes on disk, including sparse-file allocation. The `size:` search predicate uses logical file length.

**Aa** enables case-sensitive matching; **Path** searches full paths. Queries containing `/` also match full paths. **Regex** interprets the entire query as a [Rust regular expression](https://docs.rs/regex/latest/regex/), rather than Everything operators. Invalid or unsupported predicates display an error. Size units use powers of 1024.

Native macOS FSEvents tracks creates, renames, edits and removals. Ordinary directory metadata/data updates compare immediate children and scan only newly discovered subtrees. Existing descendants stay indexed. Creates, removals, renames and coalesced events rescan the affected subtree; lost events request a full rebuild. Initial and recovery scans use `getattrlistbulk` to fetch file metadata in batches, with normal filesystem reads as a fallback. A watcher failure is shown in the status area; use **Rebuild** to rescan manually.

## Access and saved data

macOS controls access to protected folders. Accept folder-access prompts or grant **Everything Mac** Full Disk Access in System Settings → Privacy & Security, relaunch, and rebuild if needed. When running through Cargo, macOS may attribute access to the terminal/developer host. A fixed exclamation icon in the bottom bar, left of Navigate, opens Full Disk Access directly in System Settings; scan-error details are available on hover. Permissions are not polled; database and watcher failures are shown separately.

The index is saved in `~/Library/Application Support/eveythingmac/index.sqlite3`. The first launch builds it; later launches show the complete saved index in its existing path order after loading, without a startup sort. Filesystem changes and permission retries are checked in the background without holding up cached results; cached entries may briefly be outdated until those checks finish. The loading indicator stays inside the table, with search, filters, column headers, and the footer visible. Each watched root keeps its own volume event checkpoint, root identity and volume-history UUID. Checkpoints and changed file rows commit in one SQLite transaction, so an interrupted save cannot move the checkpoint beyond the saved index.

Older index formats require a fresh index; no migration is performed. A full scan also runs when roots or volume histories change, history is lost, replay fails to finish, or the last verification is over seven days old. Trees spanning mounted volumes or firmlinks (for example `/`) use a host watcher and full startup verification instead of durable replay. FSEvents is advisory; **Rebuild** remains available to verify manually, including after permission changes.

The app’s own support directory is excluded to avoid a database write/update loop. Removing an indexed root prunes its saved entries. Database failures are shown in the status area; searches continue in memory, and an unrecognized or corrupt existing database is preserved.

Folder settings and window geometry are stored alongside the database under `~/Library/Application Support/eveythingmac/`. Settings writes use a private temporary file and atomic rename. Invalid existing JSON is preserved and saving is disabled until fixed. Choosing folders and searching do not rename, delete, or upload your files. **Rename…** changes the chosen result’s name; if you rename an indexed root, update its path in **Indexed folders**. **Move to Trash** explicitly moves the chosen result to macOS Trash; it never permanently deletes a result. Opening a result hands it to the system's registered application.

## What was analyzed and what differs

[The REA investigation report](docs/everything-resources.md) identifies the supplied executable as **Everything 1.4.1.1032**, records its resources and embedded help, and traces a native USN-journal query helper in Ghidra. Reproducible static evidence is saved under `docs/analysis/`. The Windows executable was not run and no Windows code or branding assets are bundled in the Mac app.

The [FSearch source review](docs/fsearch-review.md) records a complementary macOS indexing reference, the applied directory diffs, persistent FSEvents replay and bulk directory enumeration, plus remaining search/storage opportunities. Attribution is retained in [third-party notices](THIRD_PARTY_NOTICES.md).

This is a working core implementation, not full Everything feature parity:

- SQLite persists the index between launches; searches still use an in-memory copy for speed. Normal folder restarts reconcile changed paths from volume history. Whole-disk trees and recovery scans can still take time and memory. There is no NTFS MFT/USN integration.
- Only configured, accessible roots are indexed. Protected folders, offline volumes, and symlink targets can be absent. Rebuild retries watchers and rescans manually after roots are restored.
- Bookmarks, search history, content/media/date predicates, duplicate finding, thumbnails, result export, HTTP/ETP servers, Windows services, multi-window mode, and file management operations beyond copying, renaming, and moving results to Trash are not implemented.
- Wildcards match the whole filename (or whole path in path mode); Unicode case folding follows Rust regex. These semantics are tested locally, not compared to a running Windows instance. Non-UTF-8 filenames display with replacement characters, although the original path is retained for opening.

## Checks

```sh
cargo test --offline
cargo check --offline
cargo fmt --all -- --check
```

Tests cover query parsing and matching, metadata and symlink handling, live macOS create/rename/delete notifications, database round trips, offline changes across restarts, atomic checkpoint rollback, old-format rejection, quiet reopens, multiple replay roots, history-loss recovery, native bulk metadata and malformed records, cache corruption handling, settings preservation, window geometry, and the integrated search UI.
