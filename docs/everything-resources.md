# Everything.exe: resource and text evidence

This is offline inspection of the supplied Windows executable. Nothing in this report establishes that a menu was displayed or a command executed. The macOS implementation may use the embedded help as a compatibility reference, while adapting Windows-specific integration to macOS.

## Artifact and REA evidence

- File: `everything.exe`, 2,272,424 bytes; SHA-256 `f191f756996a14a11e5445fa7103d302efd510cf2fbf920e6c0c8ed51d512e36`.
- REA `inspect_pe_resources`, provider `rea-pe-resources` v1, evidence ID `ev_7ba37a6fa9e751938c1e82c341e0381f6bf22c560f85eaf57ed461cadc6f801f`.
- REA observed PE32+, machine 34404 (`0x8664`, x86-64), a complete resource tree of 52 examined entries and 23 payloads.
- Complete original evidence is [analysis/pe-resources.json](analysis/pe-resources.json). The REA evidence inventories resource structure; the following payload/text interpretation is a separate local static derivation from the same source bytes.

All hexadecimal offsets below are zero-based file offsets, not virtual addresses. Exact source text and offsets for bounded UI/help regions are retained in [analysis/ui-search-strings.json](analysis/ui-search-strings.json). Its extraction records contiguous ASCII runs, so a record alone does not establish code reachability, precedence, or active defaults.

## Identity and resources

The RT_VERSION resource is at file offset `0x224b60`, length 728, REA resource index 21, language 1033. UTF-16 fields identify CompanyName `voidtools` (`0x224c18`), FileDescription/ProductName `Everything` (`0x224c54`/`0x224da0`), OriginalFilename `Everything.exe` (`0x224d60`), and both FileVersion/ProductVersion `1.4.1.1032` (`0x224c8c`/`0x224ddc`). Fixed version fields at `0x224b88` agree with 1.4.1.1032. The copyright includes 2026; that is a string, not an authenticated build date. Raw payload: [analysis/version-resource.bin](analysis/version-resource.bin).

The complete tree contains one bitmap (type 2), eleven individual icons (type 3), eight RCDATA payloads (type 10), one icon group (type 14), one version resource (type 16), and one manifest (type 24). There are no RT_MENU or RT_DIALOG entries in this tree. Therefore menu and dialog descriptions below come from embedded program strings, not decoded native menu/dialog templates.

The manifest at `0x224e38`, length 939, declares `Everything Search Engine`, Windows Common Controls 6, `asInvoker` execution privileges, and DPI awareness. Raw payload: [analysis/application.manifest](analysis/application.manifest). It does not establish that indexing never uses elevated components.

The icon group at `0x224ac0` references eleven image payloads in the same language with matching declared sizes. REA did not decode images or observe which one the Windows shell loads.

## User interface declarations useful for the port

| Surface | Static evidence in supplied executable |
|---|---|
| Main menus | File `0x1ce428`, Edit `0x1ce420`, View `0x1cc048`, Search `0x1ce418`, Bookmarks `0x1ca6d0`, Tools `0x1ce40c`, Help `0x1ce404` |
| Search controls | Search as you type `0x1c4420`; Match Case `0x1ce328`, Match Whole Word `0x1ce310`, Match Path `0x1ce300`, Enable Regex `0x1ce2f0` |
| Results | Details `0x1bee98`, thumbnail view sizes `0x1bee18`–`0x1bee80`, preview-pane declaration `0x1c1a50`; object and selected-object count strings `0x1ce3b8`–`0x1ce3f0` |
| Sort declarations | Name `0x1c1950`, Path `0x1c1940`, Size `0x1c1930`, Extension `0x1c1918`, Type `0x1c1908`, Modified `0x1c18f0`, Created `0x1c18d8`, attributes/access/run/history options nearby; ascending/descending `0x1c17f8`/`0x1c1818` |
| File actions | Open `0x1ce1e8`, Open Path `0x1ce1f0`, Explore `0x1ce200`, Explore Path `0x1ce208`, Copy Full Name `0x1ce218`, Copy Path `0x1ce238`, Copy Name `0x1bf088`, Rename `0x1c0be8` |
| Filters | Filter-manager declarations `0x1c45c0`–`0x1c4bfe`; embedded extension lists for Audio `0x1bbe80`, Video `0x1bbb40`, Picture `0x1bbc70`, Executable `0x1bbcb8`, Document `0x1bbce0`, Compressed `0x1bbdc0` |
| Bookmarks and history | Add/organize bookmarks `0x1ca640`–`0x1ca688`; descriptions preserve query, sort, view and match settings `0x1c9e98`–`0x1ca370`; enable search history `0x1c4208`, history import/export `0x1c1610`/`0x1c1628` |
| Export | Export current result list declaration `0x1cc4a0`, Export Result List `0x1cc4d0`, Export menu text `0x1ccae8` |
| Index controls | Force Rebuild `0x1c9690`; folder monitoring declaration `0x1c8ae0`; excluded folders/wildcards `0x1ca810`–`0x1caa28`; hidden/system exclusion declarations `0x1cc4f0`–`0x1cc628` |

These are observed declarations. This inspection does not determine exact window geometry, enabled-state rules, keyboard shortcuts, complete interaction behavior, selection persistence, or precise comparison collation.

## Search language declared in embedded help

The help block begins at `0x1c67e2`, with search-syntax title at `0x1c8268`. Its raw bytes explicitly describe:

- Space means AND; `|` means OR; `!` means NOT; `< >` groups expressions; double quotes search an exact phrase (`0x1c67e2`–`0x1c683b`). This does not establish operator precedence without code analysis or runtime tests.
- `*` matches zero or more characters and `?` matches one (`0x1c683c`–`0x1c6889`). A separate option says wildcard matches can occur anywhere if whole-filename matching is disabled, with `wholefilename:` and `nowholefilename:` overrides (`0x1c23f0`).
- `file:` and `folder:` restrict kind (`0x1c6b15`, `0x1c6b2f`); `path:` matches path plus filename (`0x1c6b4d`).
- `case:`, `diacritics:`, `regex:`, `startwith:`, `endwith:`, `wholefilename:`, `wholeword:`, `wildcards:` modify matching (`0x1c6a9d`–`0x1c6c65`). Help says the `no` prefix disables a modifier (`0x1c6c97`).
- `ext:<ext1;ext2;...>` uses a semicolon-delimited extension list (`0x1c7457`–`0x1c74cc`). Category macros include audio and video (`0x1c6995`, `0x1c6a41`), with other category descriptions nearby and concrete extension lists in the program.
- `parent:<path>` and `infolder:<path>` search the specified directory excluding descendants (`0x1c7811`, `0x1c76ba`); `child:<filename>` finds folders with a matching child (`0x1c6e9e`).
- `size:<size>` filters byte size (`0x1c7a57`); function syntax declares `=`, `<`, `<=`, `>`, `>=`, and `start..end` or `start-end` ranges (`0x1c7c75`–`0x1c7dfc`). Size syntax declares `kb`, `mb`, `gb` (`0x1c7e0c`). The numeric multiplier is not established by these strings.
- Created, modified, accessed, and run dates have long and short forms: `datecreated:`/`dc:`, `datemodified:`/`dm:`, `dateaccessed:`/`da:`, `daterun:`/`dr:` (`0x1c709e`–`0x1c73cf`). Help declares locale dates, ISO-like timestamps, `today`, `yesterday`, `tomorrow`, relative periods and named months/weekdays (`0x1c7ef1`–`0x1c818c`). Parsing precision and time-zone behavior remain unknown.
- `count:<max>` limits result count (`0x1c706c`); `len:<length>` filters filename length (`0x1c7716`); `dupe:`, `sizedupe:`, and metadata duplicate predicates are declared (`0x1c740b`, `0x1c7a98`, related help).
- `content:`, explicit ANSI/UTF-8/UTF-16 content forms, media metadata, image dimensions/orientation and attributes are also declared in this help region. They expand beyond a minimal filename-search port.

## Indexing and platform boundary

Embedded help says file content is not indexed (`0x1c0740`). It separately describes optional indexed file sizes (`0x1c1540`), folder sizes (`0x1c1470`), modified timestamps (`0x1c1310`), and extra memory/performance costs of fast sort indexes (`0x1c0bf0`–`0x1c11a0`). These statements suggest a filename-first product with optional metadata indexes; they do not prove internal data structures or measured performance.

Windows-specific declarations include NTFS/USN journal monitoring (`0x1cd750`–`0x1cd950`), an administrator/service requirement for NTFS indexing (`0x1c2170`), and a warning that the service permits all local accounts to index all NTFS filenames (`0x1c3cbf`–`0x1c3d16`). ETP/FTP remote-search/server strings (`0x1ce178`, `0x1ce2b0`–`0x1ce2d0`) and HTTP-server settings are also present. None of those declarations demonstrates actual network use or current service configuration.

For macOS, a local filename index, filesystem change monitoring, Finder reveal, and native file opening are reconstruction choices that serve the corresponding workflow. This evidence does not justify claiming NTFS MFT/USN performance or complete Windows feature parity on macOS.

## Focused native-code corroboration

The parent investigation additionally imported the same executable into Ghidra 12.1.4 and exported [analysis/native-evidence.json](analysis/native-evidence.json). This is static analysis; it did not execute the Windows program. The session was closed after export.

The overview (`ev_a25f96fe1240b8c663470fab0ff980061b3f871369b72c53819cbcb797d0c2fc`) reports image base `0x140000000`, 3,070 procedures and 4,841 defined strings. These are Ghidra inventory counts, including its local/external function inventory; they do not prove discovery of every function or string.

The diagnostic string at virtual address `0x1401d2550` mentions `FSCTL_QUERY_USN_JOURNAL`. Xrefs (`ev_b945820e500983499b7c576b4f195c157e318678cde15066c006e3f6998da16e`) found four references, at `0x1400d8545`, `0x1400d9576`, `0x1400e06cb`, and `0x1400e087e`. Containment evidence `ev_cee9ce40df565c21c2babcecb6c7c303da24f99e7f8b8aecd5db7df865a324e6` places the first in `FUN_1400d84c0`, inclusive analyzed range `0x1400d84c0`–`0x1400d8577` (184 bytes).

The function dossier (`ev_0747c90db17dacbbe0a4497d0876e9e65163b952054a5a7d132cd1cb717c6e8f`) contains matching assembly, pseudocode, resolved API calls, and a conditional failure path:

- At `0x1400d84fb`, it calls imported `KERNEL32.DLL::DeviceIoControl` with the first parameter as the handle, control code `0x900f4`, no input buffer, the second parameter as an output buffer, output length `0x38`, a stack location for bytes returned, and no overlapped request.
- At `0x1400d8501`–`0x1400d8514`, it checks the result and returns 1 on success.
- Failure calls imported `KERNEL32.DLL::GetLastError` at `0x1400d851a`, then calls `FUN_1400b3fc0` with a journal-query failure diagnostic. Errors `0x49a` and `0x49b` skip a second, more detailed diagnostic; all failures return 0.
- The detailed diagnostic references the supplied handle/output buffer and explicitly labels the operation `FSCTL_QUERY_USN_JOURNAL`. Together with the call arguments, this supports interpreting the helper as a Windows USN-journal query wrapper, beyond the presence of an unused help string.

The dossier reports eight resolved caller functions, including `0x14004fb70`, `0x1400b3800`, `0x1400b3850`, `0x1400ee760`, `0x1400eea60`, `0x1400eefc0`, `0x140147290`, and `0x140147d20`. Their bodies were not traced here. This establishes a statically connected Windows journal boundary, but does not reconstruct initial volume enumeration, MFT reads, journal replay, rename handling, overflow recovery, persistence, or the search index representation. It also does not establish which path executes on a particular machine.

Ghidra reports an `Everything.pdb` reference with unknown load state; original names and types may be unavailable. The recovered parameter/return types have low confidence, pseudocode is generated rather than original source, and unresolved indirect calls remain unknown. The retained string-search evidence `ev_39309937f18f6b757248edff9555bfbd6d3cb36838b7e7275f07e131de45bfe9` supplies additional journal diagnostics, but those were not promoted into claims about an overflow algorithm without control-flow analysis.

## Verification and unresolved scope

Local verification checked that every REA resource payload range is within the supplied file and that every payload hash matches REA. It also checked fixed and string version agreement. No application process was launched, no Windows VM was used, and no native REA session was opened or changed for this resource subtask.

Skipped: runtime UI/shortcut verification and code tracing of the declared search grammar. Risk: static strings can describe inactive or conditional features; the port should document its implemented subset and validate its own behavior.
