# FSearch source review

Reviewed [noahdunnagan/fsearch](https://github.com/noahdunnagan/fsearch) at commit [`88e63fdf850cf7749deae8198b187accd21f8ba5`](https://github.com/noahdunnagan/fsearch/tree/88e63fdf850cf7749deae8198b187accd21f8ba5). This was source inspection; the upstream app, daemon, installer and benchmarks were not executed. Its advertised timings are upstream measurements, not measurements of Everything Mac.

| Area | Useful upstream approach | Everything Mac outcome |
| --- | --- | --- |
| Live folder updates | Relist one changed directory; compare immediate children; scan only newly added subtrees | Applied to ordinary directory data/metadata events |
| Restart indexing | Save an FSEvents checkpoint with the index and replay later events | Applied with volume-relative streams, atomic SQLite checkpoints and recovery scans |
| Initial scanning | Bulk macOS directory metadata reads with `getattrlistbulk` | Applied with checked native record decoding and standard filesystem fallback |
| Search and memory | Intern names, map packed arrays from disk, reject candidates using name bitmaps | Current SQLite cache and Everything-style query semantics retained |

## Applied: directory diffs

[`src/live.rs`](https://github.com/noahdunnagan/fsearch/blob/88e63fdf850cf7749deae8198b187accd21f8ba5/src/live.rs#L202) separates listing a changed folder from scanning a new subtree. Existing child directories keep their indexed descendants. Missing or replaced directories lose their entire indexed subtree. Replaying the same directory listing has no further effect.

The local indexer now follows that approach for ordinary directory data/metadata events. It preserves unchanged descendant records and their access errors, indexes newly discovered trees, and removes deleted/replaced trees. A shallow parent update does not suppress another queued event in an existing child. Creates, removes, renames, ambiguous events and overflow recovery keep recursive handling. SQLite receives the changed paths through its existing transaction path.

New tests cover unchanged descendants, repeated updates, deeper changes handled separately, new trees, deleted trees, file/directory replacements, symlink replacements, cancellation and retained access errors. Existing live-notification and database restart tests remain in the full suite. The implementation uses the local scanner and does not add an upstream runtime dependency.

## Applied: persistent FSEvents replay

[`src/fsevents.rs`](https://github.com/noahdunnagan/fsearch/blob/88e63fdf850cf7749deae8198b187accd21f8ba5/src/fsevents.rs) exposes event IDs and historical replay. [`src/engine.rs`](https://github.com/noahdunnagan/fsearch/blob/88e63fdf850cf7749deae8198b187accd21f8ba5/src/engine.rs#L285) loads the saved index before starting replay from its saved checkpoint. Its apply loop tracks dropped events and pending directories before advancing the fully applied checkpoint. It also has recovery paths for lost history and newly available permissions.

Everything Mac now commits each root's event checkpoint atomically with changed SQLite rows. Volume-relative streams map physical device paths back to configured paths, including the macOS data-volume firmlink mapping. Root paths/inodes and FSEvents volume UUIDs must match before reuse. Dropped history, changed roots, mount changes and replay timeouts recover with a full scan; old errors are retained and retried. Verification older than seven days also requests a scan. Trees that span mounts/firmlinks use the host watcher and full startup verification. This follows [Apple's FSEvents guidance](https://developer.apple.com/library/archive/documentation/Darwin/Conceptual/FSEvents_ProgGuide/UsingtheFSEventsFramework/UsingtheFSEventsFramework.html).

The native watcher replaces notify because its public Event API did not expose event IDs or a historical start position. Watchers start before full enumeration, and checkpoints never advance past unapplied callbacks. Own-database events do not trigger another save. Version-one databases migrate without discarding file rows. Tests cover offline changes, quiet reopens, multiple roots, atomic rollback (including checkpoint-only saves), migration, changed roots and forced overflow recovery.

## Applied: bulk enumeration; remaining search ideas

[`src/walk.rs`](https://github.com/noahdunnagan/fsearch/blob/88e63fdf850cf7749deae8198b187accd21f8ba5/src/walk.rs) requests names and metadata in bulk using `getattrlistbulk`, then distributes directory work across threads. Everything Mac now requests batched names, file types, logical sizes and nanosecond timestamps. Native records are bounds-checked. Directories, links and unavailable attributes use `symlink_metadata`; unsupported bulk enumeration falls back to ordinary reads. Traversal remains cancellable, keeps configured roots and does not follow symlinks. It does not import the upstream thread pool or global scan policies.

A warm-cache local check of 4,000 temporary files measured 13.05 ms for `read_dir` plus per-file metadata, versus 6.66 ms for bulk reads. This is one enumeration microbenchmark, not an end-to-end or cold-disk speed claim. Reproduce with `cargo test --offline bulk::tests::enumeration_timing -- --ignored --nocapture`.

[`src/index.rs`](https://github.com/noahdunnagan/fsearch/blob/88e63fdf850cf7749deae8198b187accd21f8ba5/src/index.rs) stores distinct names once, uses compact mapped arrays, and lays out directory children/subtrees as ranges. Name bitmaps eliminate candidates before detailed matching. These are useful for larger indexes, but adopting that storage model would replace much of our cache and search path; this review does not establish a performance benefit on our workload.

[`src/query.rs`](https://github.com/noahdunnagan/fsearch/blob/88e63fdf850cf7749deae8198b187accd21f8ba5/src/query.rs#L596) also uses different semantics: fuzzy words are the default, `re:` is a filename regex, and `regex:` searches file contents. Substituting its parser would change current Everything-style results. The current Boolean, literal, wildcard and filename-regex behavior remains covered by existing tests.

## Attribution

FSearch is MIT licensed, copyright 2026 Noah Dunnagan. The directory-diff, FSEvents replay and bulk-enumeration approaches informed this implementation. Its notice is retained in [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md).
