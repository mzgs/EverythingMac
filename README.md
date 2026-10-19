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

## GitHub release

The script requires `curl` and Python 3; GitHub CLI is not needed. It uses
the GitHub token already stored in the HTTPS origin URL in `.git/config`
(`https://USERNAME:TOKEN@github.com/OWNER/REPO.git`), without printing it.
The token needs repository access and permission to run Actions workflows.
Set the release version in `Cargo.toml`, update `Cargo.lock`
with `rustup run stable cargo check`, then commit and push your changes to GitHub.
From a clean checkout, run:

```sh
./release.sh
```

This [dispatches through GitHub's REST API](https://docs.github.com/en/rest/actions/workflows#create-a-workflow-dispatch-event)
`.github/workflows/release.yml` on the current pushed branch, with the exact current
commit as an input. The workflow must already exist on GitHub's default branch.
GitHub Actions builds macOS ARM64 and Intel x86_64 app bundles on separate runners,
packages each as a ZIP with a SHA-256 checksum, then creates a `v<version>` GitHub
release with generated notes after both builds succeed and all assets upload.
An existing tag is rejected before building. Publishing uses Actions' built-in token with `contents: write`;
your local token is only used to dispatch the workflow.
If an upload fails, the release stays a draft; remove that draft in GitHub before
retrying the same version.

The commit must already be on GitHub and the tag must be new. The script does not
commit, push, or build locally. Windows x86_64 and Linux x86_64 releases require an
app port first: indexing, file actions, clipboard, and previews currently use
macOS-only APIs. Downloads are ad-hoc signed and not notarized; macOS may block them
on other Macs.

Check the release script without building the app or contacting GitHub:
`python3 tests/release_test.py`.

## Search

Your home folder is indexed by default. Open **Everything Mac → Settings…** (`⌘,`) to add/remove folders or mounted volumes, then save. **Rebuild cache** is also available in this settings dialog to rescan the saved indexed folders. Use **Exclude folders…** in the same dialog to skip specific folders and their contents; save to apply. Exclusions also remove cached results and apply to live updates. Removing an exclusion rescans the indexed folders. Existing settings default to no custom exclusions. Nested roots are deduplicated by the indexer. Hidden files are included; symbolic links are listed without following them. Names, sizes and modified times are scanned in the background; file contents are never read by the indexer.

Cloud folders are excluded from scans, saved results, and live updates: macOS `~/Library/CloudStorage` (Google Drive, Dropbox, OneDrive, Box, and other File Provider services), iCloud’s `~/Library/Mobile Documents`, and standard home-folder locations for Dropbox, Google Drive, OneDrive, Box Sync, pCloud Drive, and MEGA. Custom sync locations outside these paths are not detected. Full recursive scans use one directory worker per available logical CPU.

Search updates as you type. Results initially use the index’s existing path order without sorting at launch. Results are virtualized and can be sorted by name, path, size, or modified time. Click a header again to reverse its sort. Filter by files, folders, audio, images, videos, or documents. Right-click a result for **Open**, **Show in Finder**, **Get Info**, **Copy**, **Copy path**, **Rename…**, and **Move to Trash**. **Get Info** opens Finder’s information window and may request permission to control Finder. **Copy** puts the actual file or folder on the clipboard for pasting in Finder. **Rename…** edits its name in place and refuses to replace an existing item. The trash action uses macOS Trash so files and folders can be restored in Finder; failures appear in the search error area. Select multiple results with `⌘`-click, select a range with Shift-click or `⇧↑`/`⇧↓`, and use `⌘A` in the file list to select all results. Right-clicking a selected result preserves the selection. **Copy** (`⌘C` in the list), **Copy paths** (`⇧⌘C`), **Open**, **Show in Finder**, and **Move to Trash** (`⌘⌫` in the list) apply to the selection. Paths are copied one per line in result order. Rename and Get Info are available for a single selection; Quick Look previews the active result. Bulk Trash skips descendants of a successfully trashed selected folder and reports failures while continuing with other selected items. Selecting a result moves keyboard focus from search to the file list. Press Space there to preview the selected result in the app’s native macOS Quick Look panel; Up/Down arrows preview the previous/next result in the current list order; Space or Escape closes the preview. Spaces still type normally in the search field. Double-click or press Return to open the selected result with its default app; `⌘Return` reveals it in Finder and `⇧⌘C` copies its full path. `⌘F` focuses search, `↑`/`↓` select results, and `⌘R` rebuilds the index.

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
| `modified:today` / `dm:yesterday` | Modified on that local calendar day |
| `modified:thisweek` | Modified Monday through Sunday of this week |
| `modified:last7days` | Modified today or during the previous six days |
| `modified:>=2026-01-01` | Modified on or after a date |
| `modified:2026-01-01..2026-01-31` | Inclusive range of local dates |

Modified-date filters also support `>`, `<`, `<=`, and `=` with `YYYY-MM-DD` dates. A bare date matches that entire day, and `>date` starts on the following day. Files and folders without a known modified time do not match a date predicate. Relative date searches refresh when the local calendar day changes.

**Saved** opens named searches. Enter a unique name and choose **Save current search** to save the query, case/path/regex/content modes, file-type filter, sort order, and folder scope. Select a saved name to restore it; **×** removes it. Up to 50 searches are stored locally in `saved-searches.json` beside the index, using private permissions and atomic writes. Invalid or unrecognized saved data is preserved and saving is disabled, with an error shown in the dropdown.

Right-click a single folder and choose **Search here** to start a fresh search of its indexed descendants. The **Within:** chip displays its full path; **×** clears the scope while keeping the query. Scope respects configured indexed folders and exclusions, including in Content mode; it never scans an unindexed location. Similar folder prefixes such as `Archive` and `Archive2` are kept separate.

Drag a result out of the window into Finder or another app to offer the actual file URLs using native macOS drag and drop. Dragging an already selected result offers the entire selection, in result order. Outbound drags offer a copy operation; the app does not remove the source files. The destination app decides how to handle a drop.

Filename search automatically allows one inserted, missing, replaced, or adjacent transposed character in unquoted plain filename terms of 4–64 characters; for example, `anual report` finds `annual_report.pdf`. Shorter/longer terms, quoted phrases, wildcards, and predicates retain their exact matching rules. Typo tolerance is disabled in Aa, Path, Regex, and Content modes. With **Aa**, letters and their case must match exactly.

Filename results are sorted automatically by exact full filename, filename prefix, other exact text matches, then fuzzy-only matches. For multiple required terms, the weakest filename term determines the tier; a complete plain-text phrase also gets exact/prefix priority. Alternatives use their best matching branch. Folder paths, exclusions, and metadata predicates only filter results; searches with no positive filename terms keep path order. Equal-ranked results keep path order, without further similarity scoring. Click a column header to choose an explicit sort; Reset search restores automatic ranking. The sort choice is included in saved searches. Path, Regex, and Content searches keep path order unless a column sort was chosen; duplicate results keep their group order. Existing saved searches still load, and their old typo-tolerance flag no longer changes filename matching.

The Size column and Size sorting use allocated bytes on disk, including sparse-file allocation. The `size:` search predicate uses logical file length.

**Duplicates** scans the current matching files on demand, respecting the query, file-type filter, folder scope, indexed folders, and exclusions. Wait for indexing and searching to finish, then click the **Find duplicate files** icon immediately left of **Settings** in the title bar. Files are grouped by logical size and a streamed fingerprint, then compared byte for byte before being shown together. Groups appear by logical file size, largest first, with a number beside each filename. Empty files can match; directories, symbolic links, and special files are ignored, hard links to the same file count once, and cloud placeholders marked as dataless by macOS are skipped. No new dependencies or persistent content index are used.

The scan runs in the background with **Cancel** and bounded read buffers; large collections can still require substantial disk reads. Unreadable, missing, or changing files are skipped, with a count and warning shown above the results. The result is a scan snapshot: live index updates remove changed or deleted members and hide groups with fewer than two remaining files, but do not find new duplicates automatically. **Back to search**, toggling **Duplicates** off, or changing the search restores normal results; run the scan again to refresh. Column sorting is disabled while comparing or showing duplicate groups to keep each group together. Saved searches store the underlying search criteria, not duplicate results.

Duplicate matching compares regular file contents (the data fork), not tags, permissions, extended attributes, or resource forks. The scan never changes or deletes files. Existing selection, Quick Look, Finder reveal, copy, rename, and explicit **Move to Trash** actions remain available; review the files you choose to remove.

Click the search icon’s dropdown to see **Search history**. Select a query to run it again, or click its **×** to remove it. Escape or clicking outside closes the dropdown. The latest 50 distinct, nonempty valid queries are saved after a one-second typing pause or when leaving search. History stores query text; the current search modes and filters still apply when recalling it. Queries are stored locally in `~/Library/Application Support/eveythingmac/search-history.json` with private file permissions. Invalid existing history is preserved and saving is disabled, with an error shown in the dropdown.

**Aa** enables case-sensitive matching; **Path** searches full paths. Queries containing `/` also match full paths. **Regex** interprets the entire query as a [Rust regular expression](https://docs.rs/regex/latest/regex/), rather than Everything operators. Invalid or unsupported predicates display an error. Size units use powers of 1024.

**Content** switches to optional file-content search through macOS Spotlight (`mdfind`). Type text such as `annual report`, or use `*` / `?` wildcards; the entire query is a content pattern, matched case-insensitively unless **Aa** is enabled. Filename operators such as `ext:` and `size:` are not interpreted in this mode, and **Path** / **Regex** are disabled until you switch Content off. An empty content query shows no results. The file-type filters, sorting, selection, Quick Look, and file actions still work. Reset search also turns Content off.

Content results are restricted to files already in the app's index, so configured folders and exclusions still apply. Spotlight must already have indexed their text: unsupported file types, Spotlight Privacy exclusions, disabled indexing, and recent edits can produce missing or delayed matches. There is no separate content index, file download, or upload; the app queries the existing local Spotlight store. Searches run in the background, cancel when superseded or the view closes, and report errors or a 15-second timeout. Repeating a query checks Spotlight again; the app does not subscribe to Spotlight index updates.

Native macOS FSEvents tracks creates, renames, edits and removals. Ordinary directory metadata/data updates compare immediate children and scan only newly discovered subtrees. Existing descendants stay indexed. Creates, removals, renames and coalesced events rescan the affected subtree; lost events request a full rebuild. Initial and recovery scans use `getattrlistbulk` to fetch file metadata in batches, with normal filesystem reads as a fallback. A watcher failure is shown in the status area; use **Rebuild** to rescan manually.

## Access and saved data

macOS controls access to protected folders. Accept folder-access prompts or grant **Everything Mac** Full Disk Access in System Settings → Privacy & Security, relaunch, and rebuild if needed. When running through Cargo, macOS may attribute access to the terminal/developer host. A fixed exclamation icon in the bottom bar, left of Navigate, opens Full Disk Access directly in System Settings; scan-error details are available on hover. Permissions are not polled; database and watcher failures are shown separately.

The index is saved in `~/Library/Application Support/eveythingmac/index.sqlite3`. The first launch builds it; later launches show the complete saved index in its existing path order after loading, without a startup sort. Filesystem changes and permission retries are checked in the background without holding up cached results; cached entries may briefly be outdated until those checks finish. The loading indicator stays inside the table, with search, filters, column headers, and the footer visible. Each watched root keeps its own volume event checkpoint, root identity and volume-history UUID. Checkpoints and changed file rows commit in one SQLite transaction, so an interrupted save cannot move the checkpoint beyond the saved index.

Older index formats belonging to this app automatically recreate the cache tables and clear the checkpoint in one transaction, then rebuild and save a fresh index. This applies to future format upgrades as well. Your files and settings are unaffected. Newer or unrecognized database formats are preserved and saving is disabled. A full scan also runs when roots or volume histories change, history is lost, replay fails to finish, or the last verification is over seven days old. Trees spanning mounted volumes or firmlinks (for example `/`) use a host watcher and full startup verification instead of durable replay. FSEvents is advisory; **Rebuild** remains available to verify manually, including after permission changes.

The app’s own support directory is excluded to avoid a database write/update loop. Removing an indexed root prunes its saved entries. Database failures are shown in the status area; searches continue in memory, and an unrecognized or corrupt existing database is preserved.

Folder settings and window geometry are stored alongside the database under `~/Library/Application Support/eveythingmac/`. Settings writes use a private temporary file and atomic rename. Invalid existing JSON is preserved and saving is disabled until fixed. Choosing folders and searching do not rename, delete, or upload your files. **Rename…** changes the chosen result’s name; if you rename an indexed root, update its path in **Indexed folders**. **Move to Trash** explicitly moves the chosen result to macOS Trash; it never permanently deletes a result. Opening a result hands it to the system's registered application.

## What was analyzed and what differs

[The REA investigation report](docs/everything-resources.md) identifies the supplied executable as **Everything 1.4.1.1032**, records its resources and embedded help, and traces a native USN-journal query helper in Ghidra. Reproducible static evidence is saved under `docs/analysis/`. The Windows executable was not run and no Windows code or branding assets are bundled in the Mac app.

The [FSearch source review](docs/fsearch-review.md) records a complementary macOS indexing reference, the applied directory diffs, persistent FSEvents replay and bulk directory enumeration, plus remaining search/storage opportunities. Attribution is retained in [third-party notices](THIRD_PARTY_NOTICES.md).

This is a working core implementation, not full Everything feature parity:

- SQLite persists the index between launches; searches still use an in-memory copy for speed. Normal folder restarts reconcile changed paths from volume history. Whole-disk trees and recovery scans can still take time and memory. There is no NTFS MFT/USN integration.
- Only configured, accessible roots are indexed. Protected folders, offline volumes, and symlink targets can be absent. Rebuild retries watchers and rescans manually after roots are restored.
- Media/creation-date predicates, thumbnails, result export, HTTP/ETP servers, Windows services, multi-window mode, and file management operations beyond copying, renaming, and moving results to Trash are not implemented.
- Wildcards match the whole filename (or whole path in path mode); Unicode case folding follows Rust regex. These semantics are tested locally, not compared to a running Windows instance. Non-UTF-8 filenames display with replacement characters, although the original path is retained for opening.

## Checks

```sh
cargo test --offline
cargo check --offline
cargo fmt --all -- --check
```

Tests cover duplicate grouping and byte confirmation (including fingerprint collisions, empty files, hard links, symlinks, special files, cancellation, changing/missing files and live invalidation), content-query escaping, Spotlight output decoding/cancellation/errors, content-mode controls, query parsing and matching, local modified-date boundaries, fuzzy matching, saved-search validation/persistence/restoration, folder scope, drag payloads and selection, metadata and symlink handling, live macOS create/rename/delete notifications, database round trips, offline changes across restarts, atomic checkpoint rollback, old-format rebuilds, unknown-format rejection, quiet reopens, multiple replay roots, history-loss recovery, native bulk metadata and malformed records, cache corruption handling, settings preservation, window geometry, and the integrated search UI.
