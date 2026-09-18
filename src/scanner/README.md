# Scanner

The Windows scanner builds a `FileNode` tree through either the bulk directory walker (`windows.rs`)
or the raw NTFS MFT reader (`windows_ntfs.rs`). The walker is the default: it enumerates directories
in parallel with `FILE_ID_BOTH_DIR_INFO`, receiving batches of names, identities, and metadata.
The MFT reader reconstructs the tree from volume-wide metadata. It optimises eligible whole-volume
scans, but its visibility and directory attribution do not always match the walker's.

## What the MFT reader reads

NTFS's Master File Table, `$MFT`, contains fixed-size records holding typed attributes for names,
parents, data storage, and reparse information. Record 0 describes `$MFT`; record 5 is the volume root.
Resident attributes store values inside the record; non-resident attributes describe external storage.
Extension records hold attributes that do not fit in a file's base record.

`$FILE_NAME` attributes supply names and parent references. Children can precede parents, and several
records can describe one file, so parsing must precede tree construction. Deleted records remain reusable
slots: a large table can require substantial processing even when relatively few files remain.

The walker lets Windows interpret NTFS and check access as it traverses directories. Raw reading avoids
much of that interaction but takes responsibility for parsing, identity, and visibility. Reading is
sequential in logical `$MFT` order, across potentially scattered physical extents. Reading the whole table
for a small subtree would mostly discard the work, so automatic MFT selection requires a whole-volume scan.

## Choosing a scanner

Automatic selection requires a local fixed NTFS drive, raw-volume access (normally elevation), and a
whitelisted bus. Only NVMe is currently whitelisted. Unsupported cases, failed eligibility or bus queries,
unknown buses, and propagated MFT scan errors select the walker. Mounted-folder roots are rejected:
turning `C:\Mount\vol\` into `\\.\C:` would open the host drive and could scan the wrong volume.

| Value | Behaviour |
|---|---|
| `auto` | Default. Apply eligibility checks and the measured bus whitelist. |
| `off` | Use the directory walker. |
| `force` | Skip the bus whitelist, but retain the eligibility checks. |

`DISK_CLEANER_MFT` is trimmed, lowercased, and read once per process. Compare modes in separate processes;
`scan_only` prints `scanner=mft|walker` to confirm which implementation actually ran.
Individual record parse failures **do not trigger fallback**: they raise the warning badge and can leave an incomplete tree.

## Why the gate is conservative

Four NTFS volumes on one machine produced these results:

| volume | bus | `$MFT` | files | walker | MFT | result |
|---|---|---|---|---|---|---|
| C: | NVMe | 6.26 GB | 5.83M | 7,837 ms | 5,887 ms | 25% faster |
| T: | NVMe | 1.79 GB | 1.70M | 2,501 ms | 2,292 ms | 8% faster |
| D: | SATA | 3.10 GB | 2.74M | 3,038 ms | 9,435 ms | **3.1x slower** |
| E: | SATA | 760 MB | 479k | 388 ms | 2,218 ms | **5.7x slower** |

On the measured NVMe devices, MFT scanning is **parse-bound, not disk-bound**: 0.78-1.06 GB/s,
or roughly 1.0-1.4 us/file against the walker's 0.8-1.5 us/file. Sector fixups, attribute decoding,
name conversion, record merging, and tree construction cost time that more disk bandwidth cannot remove.
The near-tie is decided by device latency, with upside near 25% and downside of several times slower.
That ceiling describes this implementation and these measurements; parser improvements could change it.

`bus_favours_mft` is a whitelist of measured winners, not a general cost model. Four volumes cannot
establish predictions across controllers, cache states, and workloads. Counting occupied slots through
`$MFT`'s `$BITMAP` was considered: including volume shape gives predicted ratios of 0.66/0.65/2.39/3.36
for C:/T:/D:/E:, changing no selection decisions. The measured bus gap is 3.4x; shape contributes at most
1.5x. A high-free-fraction NVMe volume could change that balance and should be measured before adding shape.

A prefix throughput probe was tried and reverted. Reading part of `$MFT` costs time and evicts metadata
useful to the walker, damaging the fallback before selecting it. D:, E:, and T: became worse than with
no gate. Prediction must avoid trial reads that consume the alternative scanner's useful cache.

## Reading and validating records

The reader queries sector, cluster, and record sizes plus `$MFT` valid data length to bound reads and
calculate positions. The production builder prefers raw-volume reads: record 0's unnamed `$DATA`
runlist maps virtual cluster numbers (VCNs), positions within the stream, to logical cluster numbers
(LCNs), positions on the volume, using run lengths and signed LCN deltas.

Record 0's runs must cover the entire valid data length. A fragmented `$MFT` may put further runs in
extension records referenced by an attribute list; bootstrap does not follow that list. Insufficient
coverage is rejected for walker fallback rather than silently losing all later records.

Raw reads use two overlapped slots with 16 MiB buffers; an alternative file-handle path uses 8 MiB
chunks and a bounded channel of depth 2. Partial records carry across volume-read boundaries.
Windows can keep writing after `ReadFile` returns, so buffers, slots, and `OVERLAPPED` state remain alive
until completion. Cancellation and record-limit exits drain queued reads before releasing storage.

An update sequence array (USA) detects inconsistent multi-sector writes. NTFS saves each sector's final
two bytes and replaces them with a shared marker. Before decoding attributes, the parser checks those
markers and restores the saved bytes in memory; it never writes repairs to the volume.
For 1,024-byte records on 512-byte sectors, the specialised path checks bytes 510-511 and 1022-1023:
three two-byte USA entries hold one marker and two replacements. Other geometries use a generic path
that validates array bounds and trailer positions. Mismatches are parse errors; never-written zeroed
records count as unused. A successful fixup neither validates every field nor makes the scan a snapshot.

Record identities come from logical stream positions, `base + index`, continued across chunks and runs.
Unused slots still count. A short read or bad runlist could otherwise renumber plausible records and
misdirect parent references. NTFS 3.1's record-number stamp at header offset `0x2C` independently checks
position: a nonzero mismatch is a parse error; zero stamps are unchecked.

File references also contain a 48-bit record number and a 16-bit sequence number identifying successive
uses of a slot. This sequence is distinct from the USA marker. Parent and base references discard it,
so the positional check cannot detect a correctly positioned slot reused for a different file.

## Building the tree and accounting for size

Rayon fixes up and parses chunks in parallel, then fragments merge by base-record identity.
Extensions contribute names and sizes; base-record sizes take precedence. References are bounds-checked
before indexing dense tables. Name handling prefers Win32-compatible names, suppresses DOS-only aliases
when better names exist, deduplicates names, and caps file names by link count. Directories get at most one name.

Detected name-surrogate reparse entries, such as junctions, are omitted because they redirect name
resolution. Detection covers resident attributes only. A compact child index groups entry indices
contiguously by parent, avoiding per-directory hash lookups and allocations. The root is excluded from
its own children; a shared atomic visited bitmap prevents recurring directory cycles.

The application skips the diagnostic index's preliminary subtree rollup because `FileNode` computes
totals itself. Diagnostic and final totals represent different accounting stages, especially before
hardlink deduplication. Displayed file sizes use **allocation**, matching the walker and #99, rather than
logical length; allocation granularity, sparse holes, and compression can make those quantities differ.

Sparse and compressed `$DATA` spans its whole VCN range. Its `AllocatedSize` counts holes as written,
so the parser uses `TotalAllocatedSize` for physical storage instead:

| Field or check | Meaning in this parser |
|---|---|
| `AllocatedSize`, `0x28` | Allocation used for ordinary non-resident data. |
| Logical size, `0x30` | Logical length of the non-resident stream. |
| `TotalAllocatedSize`, `0x40` | Physical allocation used for sparse or compressed data. |
| `attr_len >= 0x48` | Required non-resident attribute length, including the eight-byte field at `0x40`. |

These are attribute offsets, unlike record-header offset `0x2C`. Sizes come from the unnamed `$DATA`
extent with lowest VCN zero. Resident data uses value length for logical size, rounded to an eight-byte
boundary for allocation. This convention excludes some filesystem overhead: directory totals sum
descendant allocation, not the directories' own metadata storage.

### Hardlink attribution is a policy

If `A\large.bin` and `B\large.bin` name the same file, neither name is the original. Charging both
folders makes their contents look complete but doubles the volume allocation; charging one preserves
an additive total but requires an ownership rule. The MFT tree chooses the lexicographically smallest
reachable volume-relative path before parallel construction. Choosing a filtered-out name could lose
the allocation; letting tasks race previously moved sizes among **636 directories** on an unchanged drive.
Other names remain hardlink entries contributing zero bytes, so entry counts differ from distinct-file counts.

Deleting `A` may free nothing while `B\large.bin` remains. Removing both names can release allocation
once lifetime conditions such as open handles permit it. Folder size therefore is not recoverable space.

## Visibility and benchmarking

The scanner filters raw root entries against a Win32 root listing, hiding NTFS metafiles and anything
else absent from that listing. Listed root directories that cannot be opened remain empty and zero-sized,
with an access failure recorded. `SeBackupPrivilege` is suspended for the check and its previous state
restored afterwards. This approximates ordinary visibility only at the root boundary.

The scanners evict each other's caches: a run is roughly twice as slow after the other scanner.
Alternating A/B runs therefore measure switching penalties. `benches/ab_pairs.ps1` cannot reliably
compare these implementations and also aborts on their legitimate file-count difference. Use consecutive
blocks, discard each block's first run, and counterbalance order in multiples of four, such as A, B, B, A.
Confirm `scanner=mft|walker`. First scans of occasional sessions matter most, but **are not measured here**;
these results establish no first-scan benefit.

For correctness, use `mft_diff` through `internal-tools` to dump canonical sorted listings under different
`DISK_CLEANER_MFT` values. Compare attribution, since errors can cancel in totals: on T:, totals agree
on **617.7 GB within 392 KB**, yet **2,298 of 211,329 directories** differ, by as much as **11.2 GB**.

## Known gaps

**Hardlink placement differs.** The walker charges the first name encountered; MFT chooses a path in advance.
A common policy requires retaining identity and allocation through walker finalisation. Matching totals do not imply matching folders.

**Recoverable space is missing.** Honest deletion estimates need exclusive and shared allocation across the
whole selection. Selecting both linked folders can free bytes that neither folder would free independently.

**ACL parity stops at root children.** Blocked directories deeper inside accessible subtrees are read through.
Elevated MFT scans report about **0.016% more** than the walker on volumes containing them.

**Record reuse can misattach children.** Discarding reference sequence numbers allows a directory slot reused
mid-scan to acquire the old occupant's children. There is no snapshot; `0x2C` checks position, not identity.

**Parse failures can remove subtrees.** A dropped directory record can orphan every descendant. The warning
badge, error counts, and sampled error kinds expose failures but neither recover missing contents nor trigger fallback.

**Non-resident reparse data is ignored.** A junction target exceeding roughly **700 bytes** can live in an external
`$REPARSE_POINT` value, leaving the junction treated as an ordinary directory. The threshold is approximate, not a format limit.

**Hidden attributes can be stale.** NTFS does not keep the `$FILE_NAME` hidden-bit copy current.
Treating dot-prefixed names as hidden is an application convention and does not repair stale attribute data.

**Probes can diverge.** Probe binaries duplicate raw reading and attribute decoding and have already diverged once.
Decoder changes need review in both implementations; successful probes do not replace direct production comparisons.
