# Everything Mac

Find files and folders on your Mac as you type. Search by name, filter by file type, preview results with Quick Look, and open them in your usual apps.

## Install

1. Download a ZIP from [Releases](https://github.com/mzgs/EverythingMac/releases): **arm64** for Apple Silicon or **x86_64** for Intel Macs. Check **Apple menu → About This Mac** if you're unsure.
2. Unzip it and drag **Everything Mac.app** into **Applications**.
3. Open the app. Your home folder is indexed automatically; the first scan may take a little time.

The app is not notarized, so macOS may block the first launch. If that happens, approve opening it in **System Settings → Privacy & Security**.

## Choose where to search

Open **Everything Mac → Settings…** (`⌘,`) to add folders or mounted drives, remove locations, or choose **Exclude folders…**. Save your changes to apply them.

- Hidden files are included. Symbolic links appear in results, but their targets are not scanned through the links.
- Cloud folders such as iCloud Drive, Dropbox, Google Drive, and OneDrive are excluded from their usual locations. Custom cloud sync locations may still be indexed.
- For missing protected folders, allow macOS access prompts or grant **Everything Mac** **Full Disk Access** in **System Settings → Privacy & Security**. Relaunch the app, then choose **Rebuild cache** in Settings.

## Search

Type part of a filename to start. Results update as you type, and ordinary filename searches tolerate small typos. Use the file-type filter to narrow results to files, folders, audio, images, videos, or documents. Click a column heading to sort; click it again to reverse the order.

| Search | Finds |
| --- | --- |
| `report budget` | Names containing both words |
| `"annual report"` | Names containing that exact phrase |
| `*.pdf \| *.docx` | PDF or Word documents |
| `report !backup` | Names containing “report” but not “backup” |
| `ext:jpg;png` | JPG and PNG images |
| `file:` / `folder:` | Only files or only folders |
| `path:Projects` | Matches in the full path |
| `size:>10mb` | Files larger than 10 MiB |
| `modified:today` | Items changed today |
| `modified:last7days` | Items changed today or in the previous six days |
| `modified:2026-01-01..2026-01-31` | Items changed within a date range |

Use **Aa** for case-sensitive matching, **Path** to search full paths, or **Regex** for regular expressions. Quoted phrases, wildcards, and these modes use exact matching rather than typo tolerance.

To search inside files, enable **Content** and enter text. This uses your Mac's existing Spotlight index, so some file types and recent edits may be missing. Results stay within your indexed folders. Filename filters such as `ext:` and `size:` do not apply in Content mode.

### Reuse or narrow a search

- **Saved searches:** open **Saved**, enter a name, and choose **Save current search**. Select it later to restore its query, filters, folder scope, and sorting. Use **×** to remove it.
- **Search history:** open the dropdown beside the search icon to run a recent query again. Your current modes and filters still apply.
- **Search one folder:** right-click a folder and choose **Search here**. Clear the **Within:** chip with **×** to search all indexed locations again.

## Work with results

Double-click a result or press `Return` to open it. Right-click for **Show in Finder**, **Get Info**, **Copy**, **Copy path**, **Rename…**, or **Move to Trash**.

Use `⌘`-click to select several items, Shift-click to select a range, or `⌘A` in the results list to select all. Copy, open, reveal, and trash actions work on the selection. You can also drag selected files into Finder or another app.

### Find duplicates

Narrow your search if needed, wait for indexing to finish, then click **Find duplicate files** beside **Settings**. Matching files appear in groups, with the largest files first. Scanning can take time; use **Cancel** to stop it.

The scan never deletes anything automatically. Preview and review the files before choosing **Move to Trash**. Duplicate matching compares file contents; tags and other metadata can differ. Run the scan again to refresh the groups, or choose **Back to search** to leave duplicate results.

## Keyboard shortcuts

Shortcuts for results apply when the results list has focus. Press Space there to use Quick Look; in the search field, it types a space.

| Shortcut | Action |
| --- | --- |
| `⌘F` | Focus search |
| `↑` / `↓` | Select the previous or next result |
| `Return` | Open selected items |
| `Space` | Preview the active item with Quick Look |
| `Escape` or `Space` in Quick Look | Close the preview |
| `⌘Return` | Show selected items in Finder |
| `⌘C` | Copy selected files for pasting in Finder |
| `⇧⌘C` | Copy selected paths |
| `⌘⌫` | Move selected items to Trash |
| `⌘R` | Rebuild the index |
| `⌘,` | Open Settings |

## Missing or outdated results?

Check that the folder is included in Settings, is not excluded, and that any external drive is connected. Allow access to protected folders, then use **Rebuild cache** (`⌘R`) to scan again. Cloud folders in their usual locations are excluded. For Content searches, Spotlight must have indexed the file's text.

The app saves its index between launches and updates it as files change. Saved results may briefly be outdated while the app checks for changes after opening.

## Your files and privacy

Searches run locally. The app does not upload your files. Normal filename search indexes names and file details; **Content** uses Spotlight, and **Find duplicate files** reads file contents to compare them.

Settings, search history, saved searches, and the index are stored in `~/Library/Application Support/eveythingmac/`.

Searching does not change your files. **Rename…** changes an item's name, and **Move to Trash** moves it to macOS Trash, where you can restore it in Finder. If you rename an indexed root folder, update its path in Settings.
