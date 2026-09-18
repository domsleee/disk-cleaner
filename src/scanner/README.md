# Scanner

The Windows scanner has two ways to build the application's `FileNode` tree.
They share an output type and aim to report the same view of a volume, but they
obtain their information differently. Their totals and directory attribution
are not identical in every case.

The **bulk directory walker**, in `windows.rs`, is the default and general-purpose
implementation. It enumerates directories in parallel using
`FILE_ID_BOTH_DIR_INFO`, which returns batches of directory entries with file
identity and metadata. It handles the scan cases that the MFT reader cannot.

The **raw NTFS MFT reader**, in `windows_ntfs.rs`, reads NTFS's central file
metadata table and reconstructs the directory tree in memory. It is an
optimisation for whole-volume scans on eligible devices, not a replacement for
the walker. Understanding its tradeoffs requires separating three questions:
how metadata is read, how names become a tree, and what a directory's size means.

## What the MFT contains

NTFS stores information about files and directories in the **Master File Table**,
usually written `$MFT`. This is itself an NTFS metadata file. Its contents are
a sequence of fixed-size file records, including records describing NTFS's own
metadata. Record 0 describes `$MFT`; record 5 represents the volume's root.

A file record is not the file's contents in the usual sense. It holds a header
and a collection of typed **attributes**. Attributes describe such things as
names, parent directories, data storage, and reparse information. Small values
can live inside the record, while larger values are stored elsewhere on disk.

NTFS calls a value stored inside its record **resident**. A **non-resident**
attribute instead describes storage outside the record. A small file's data can
be resident; a larger file's `$DATA` attribute describes its external storage.
If a file's attributes do not fit in one record, extension records can hold more
of them and refer back to the file's base record.

A `$FILE_NAME` attribute supplies a name and a reference to its parent directory.
That gives the raw scanner the edges needed to reconstruct paths. Reading records
does not yield a directory tree in traversal order: children can arrive before
parents, and several records can contribute attributes to the same file.

Records also remain as reusable slots after files are deleted. Consequently,
the amount of `$MFT` to read is not determined solely by today's live file count.
A volume with many unused records can require substantial reading and inspection
for comparatively few entries in the final tree.

### Why this differs from walking directories

A directory walker begins at a path, asks Windows for its children, and repeats
that operation for each directory it can enter. Windows performs the NTFS
interpretation and access checks. Bulk enumeration and parallel work reduce
overhead, but the work still follows the directory hierarchy.

The MFT reader instead consumes a volume-wide metadata stream. It validates and
decodes records, collects names and parent references, and builds the hierarchy
afterwards. It avoids much of the per-directory interaction with Windows, but
takes on format parsing, identity handling, and visibility decisions itself.

“Sequential” refers to the logical order of `$MFT`, not a promise that the file
occupies one continuous region of the device. Like other large files, `$MFT`
can have multiple physical extents. The reader must follow those extents in the
correct order to preserve the meaning of each record's position.

This explains the whole-volume restriction. Reading all of `$MFT` to display
one small subtree would pay for metadata that the result mostly discards.
A walker can begin at the requested directory and limit its work accordingly.

## Choosing a scanner

Automatic selection requires a whole-volume scan on a local fixed NTFS drive,
with permission to open the raw volume, normally through elevation. The bus
must also be on the performance whitelist. An unsupported case, failed
eligibility query, or propagated MFT scan error sends the scan to the walker.

The current whitelist contains NVMe only. An unknown bus or a failed bus query
keeps the walker. The implementation also rejects mounted-folder volume roots:
deriving `\\.\C:` from a root such as `C:\Mount\vol\` would open the host drive,
which could mean scanning the wrong volume.

`DISK_CLEANER_MFT` controls selection:

| Value | Behaviour |
|---|---|
| `auto` | Default. Apply eligibility checks and the measured bus whitelist. |
| `off` | Use the directory walker. |
| `force` | Skip the bus whitelist, but retain the eligibility checks. |

The environment value is read once per process, trimmed, and lowercased.
Use separate process runs when changing modes for comparisons. The `scan_only`
tool prints `scanner=mft|walker`, so a benchmark can confirm which scanner
actually ran instead of assuming that a requested mode succeeded.

There is an important exception to “errors fall back”: individual record parse
failures are accumulated and reported through the warning badge. They do not
currently abort the MFT scan. A tree returned with those warnings can be
incomplete, as described under Known gaps.

## Why the performance gate is conservative

Reading a central table sounds as though it should always beat walking many
directories. Measurements do not support that conclusion. These results come
from four NTFS volumes on one machine:

| volume | bus | `$MFT` | files | walker | MFT | result |
|---|---|---|---|---|---|---|
| C: | NVMe | 6.26 GB | 5.83M | 7,837 ms | 5,887 ms | 25% faster |
| T: | NVMe | 1.79 GB | 1.70M | 2,501 ms | 2,292 ms | 8% faster |
| D: | SATA | 3.10 GB | 2.74M | 3,038 ms | 9,435 ms | **3.1x slower** |
| E: | SATA | 760 MB | 479k | 388 ms | 2,218 ms | **5.7x slower** |

The crux is that the MFT path is **parse-bound, not disk-bound** on the measured
NVMe devices. It achieves 0.78-1.06 GB/s, far below what those disks can deliver.
That works out at roughly 1.0-1.4 us/file, against the walker's 0.8-1.5 us/file.

The raw bytes still need sector fixups, attribute decoding, name conversion,
record merging, and tree construction. Increasing disk bandwidth cannot remove
that work. Once processing limits throughput, supplying bytes faster offers
little benefit unless the processing cost also falls.

The measured contest is therefore a near-tie decided by device latency, with
an upside capped near 25% in these results and a downside of several times
slower on the wrong device. A faster disk does not by itself widen the win.
This is an empirical ceiling for this implementation and these measurements,
not a proof that future parser changes cannot improve it.

### Why the gate is a whitelist, not a cost model

`bus_favours_mft` records buses measured to win. It does not claim to predict
every volume's performance from first principles. Four volumes on one machine
are useful evidence for a conservative gate, but limited evidence for a general
model across devices, controllers, cache states, and workloads.

Volume shape was considered. `$MFT` has a `$BITMAP` attribute indicating which
record slots are in use. Counting those slots helps distinguish a densely used
table from one containing substantial free space.

Including that information moves the predicted ratios to
0.66/0.65/2.39/3.36 for C:/T:/D:/E:, respectively, but changes none of the four
selection decisions. In these measurements the bus gap is 3.4x, whereas the
shape term is at most 1.5x.

A high-free-fraction NVMe volume is the case where shape could begin to matter:
the bus favours MFT scanning, but many record slots produce no useful entries.
Build and measure that case before adding shape to the gate.

### Why a throughput probe was abandoned

Measuring achieved throughput from a prefix of `$MFT` was tried and reverted.
The probe consumes time and changes the state in which the selected scanner
will run. In particular, reading the prefix evicts metadata cached for the
walker, which may then be selected as the fallback.

This made D:, E:, and T: worse than having no gate at all. The decision procedure
damaged the alternative it was supposed to select when appropriate. The
prediction therefore has to be free in this sense: it must not perform a trial
MFT scan that consumes the walker's useful cache.

## Reading and validating the metadata stream

The reader queries NTFS for the sector size, cluster size, file-record size,
and `$MFT` valid data length. It uses those values to bound reading and calculate
record positions rather than assuming every volume has identical geometry.

A **cluster** is a filesystem allocation unit. A **virtual cluster number**
(VCN) identifies a cluster's position within a file's stream; a **logical cluster
number** (LCN) identifies its position on the volume. A runlist maps consecutive
ranges of the former onto ranges of the latter.

The production index builder prefers raw-volume reading. It obtains record 0,
decodes the runlist for `$MFT`'s unnamed `$DATA`, and reads the corresponding
volume extents. Runlist entries encode lengths and signed changes in LCN, so
decoding them is necessary before file offsets can become volume offsets.

The runs found in record 0 must cover the entire valid data length. A sufficiently
fragmented `$MFT` can put additional extent information in extension records
referenced through an attribute list. This bootstrap path does not follow that
list. It rejects insufficient coverage so the caller can use the walker,
instead of accepting a scan missing every later record.

Raw-volume reading uses two overlapped slots with 16 MiB buffers. The next read
can proceed while the current buffer is processed. An alternative file-handle
path uses 8 MiB chunks and a bounded channel of depth 2 between reading and
processing. Partial records are carried forward across volume-read boundaries.

Overlapped I/O also imposes an ownership constraint: Windows may still be writing
into a buffer after `ReadFile` returns. The slot, buffer, and `OVERLAPPED` state
must remain alive until completion. Cancellation and record-limit exits drain
queued reads before releasing that storage.

### Update sequence arrays and sector trailers

An MFT record can span several sectors. A write interrupted partway through
could leave some sectors from one version of the record and others from another.
NTFS uses an **update sequence array** (USA) to help detect this kind of
inconsistent multi-sector record.

Before writing the record, NTFS saves the original final two bytes of each
sector in the array and replaces those bytes with a shared marker. The array
contains the marker followed by the saved trailer values. On reading, matching
markers provide a consistency check across the protected sectors.

Those marker bytes occupy positions that otherwise contain record data.
Consequently, the parser must check the markers and restore the saved bytes
before decoding attributes. This “fixup” changes the in-memory read buffer;
it does not write repairs to the volume.

For a 1,024-byte record on 512-byte sectors, the specialised path checks trailers
at bytes 510-511 and 1022-1023. The USA has three two-byte entries: one marker
and two replacements. A generic path handles other geometries and validates
array bounds and trailer positions.

A mismatch becomes a parse error. Passing this check does not prove every field
is valid, and it does not make the scan a snapshot. Never-written, zeroed records
are treated as unused space rather than corruption.

### Positional identities and the check at 0x2C

The index assigns record numbers from positions in the logical `$MFT` stream:
`base + index` within each chunk, continued across chunks and data runs.
Unused records still occupy positions. Skipping an unused record's contents
must not remove its slot from the numbering.

This is dangerous because a short read or bad runlist can make later records
appear at the wrong positions. Their contents might still look plausible, while
every parent reference now connects to the wrong entry.

NTFS 3.1 records carry their own record number at header offset `0x2C`.
The parser compares a nonzero stamped number with the calculated position and
reports a mismatch as a parse error. Zero stamps are not checked. This provides
an independent check against silent renumbering, not a complete integrity proof.

A different sequence number addresses a different problem. An NTFS file
reference contains a 48-bit record number and a 16-bit sequence number.
The record number identifies a reusable slot; the sequence number distinguishes
successive uses of that slot, so an old reference need not identify its new owner.

That identity sequence number is distinct from the USA's sector marker.
This implementation discards the sequence component of parent and base
references. A positional stamp can detect the wrong slot, but cannot detect
the right slot being reused for a different file during the scan.

## Turning records into a tree

Chunks are fixed up and parsed in parallel with Rayon. Their fragments are
then merged by base-record identity. Extension records contribute names and
data sizes to their owner; a size from the base record takes precedence when
available. Reference bounds are checked before indexing dense tables.

Name handling must distinguish hardlinks from alternate spelling conventions.
NTFS can store both a long name and a DOS short-name alias. The implementation
prefers Win32-compatible names, avoids emitting DOS-only aliases when better
names exist, removes duplicates, and caps materialised file names using the
record's link count. Directories materialise at most one name.

Detected name-surrogate reparse entries are omitted. Such entries redirect name
resolution, as junctions do, rather than describing ordinary children to include
in this volume tree. Detection currently covers resident reparse attributes only.

A compact child index stores entry indices in contiguous ranges by parent
record. This avoids a hash lookup and a separate allocation for every
directory. The root's self-parenting record is excluded from its own children,
and a shared atomic visited bitmap prevents directory cycles from recurring.

The application scan skips the diagnostic index's preliminary subtree rollup
because the final tree computes sizes itself. Diagnostic index totals and
finished `FileNode` totals are therefore different stages of accounting,
particularly before hardlink allocation is deduplicated.

## What the sizes mean

Displayed file sizes are **allocated sizes**, not logical lengths, matching
the walker and the decision in #99. Logical length describes the stream's
addressable contents. Allocation describes storage assigned to it, which can
differ because of allocation granularity, sparse holes, or compression.

Sparse and compressed `$DATA` attributes span their whole VCN range. Their
`AllocatedSize` field at attribute offset `0x28` counts holes as though written;
using it would overstate physical storage. Their physical bytes are recorded
in `TotalAllocatedSize` at `0x40`.

| Field or check | Meaning in this parser |
|---|---|
| `AllocatedSize`, `0x28` | Allocation used for ordinary non-resident data. |
| Logical size, `0x30` | Logical length of the non-resident stream. |
| `TotalAllocatedSize`, `0x40` | Physical allocation used for sparse or compressed data. |
| `attr_len >= 0x48` | Required non-resident attribute length, including the eight-byte field at `0x40`. |

These offsets are relative to the attribute, unlike the record-header offset
`0x2C`. The parser takes sizes from the unnamed `$DATA` extent whose lowest
VCN is zero. For resident data, it uses the value length as logical size and
rounds that length up to an eight-byte boundary for allocation accounting.

This is the scanner's file-size convention, not an accounting of every byte of
filesystem overhead. Directory sizes sum the allocation attributed to their
descendants; they are not measurements of the directories' own metadata storage.

### Hardlinks make folder sizes a policy choice

A hardlink is another directory entry naming the same file. At the record level,
multiple `$FILE_NAME` attributes can associate one base-record identity with
different names and parents. These names share data and allocation; they are
not independent copies and no name is inherently the original.

Suppose `A\large.bin` and `B\large.bin` name one file. Charging its full allocation
to both folders makes each folder's contents look complete, but double-counts
the volume total. Charging it to only one preserves an additive total, but
requires a policy to choose which folder gets the bytes.

The MFT tree counts the allocation once. Before parallel tree construction,
it chooses the lexicographically smallest volume-relative path among names
the tree will reach. Reachability matters: choosing a name hidden by root
filtering could otherwise remove the allocation from the displayed tree.

This choice must precede parallel work. An earlier approach let racing tasks
decide which name claimed the bytes. On an unchanged drive, that moved sizes
between **636 directories** across scans. Deterministic ownership removes that
scheduling dependency without pretending one hardlink truly owns the storage.

Other emitted names remain file entries, marked as hardlinks, but contribute
zero bytes. Counting file entries and counting distinct allocated files are
therefore different operations.

Deleting `A` in the example may free none of this file's allocation because
`B\large.bin` still names it. Deleting both names can release the allocation
once remaining filesystem lifetime conditions, such as open handles, permit it.
A displayed directory size cannot therefore be read as recoverable space.

## Visibility and access checks

Raw volume access exposes metadata without visiting each directory through the
ordinary Win32 path. The scanner must deliberately reconstruct the user-visible
boundary that directory enumeration would otherwise provide.

It lists the root through Win32 and filters raw entries against that listing,
hiding NTFS metafiles and anything else absent from it. Root directories that
are listed but cannot be opened remain visible as empty, zero-sized directories.
The access failure is recorded.

`SeBackupPrivilege` is temporarily suspended during this check so backup access
does not turn an otherwise blocked directory into an accessible one. The guard
restores the previous privilege state afterwards. This mirrors ordinary listing
behaviour only at the root boundary, not at every depth.

## Benchmarking this implementation

The scanners evict each other's caches. A run is roughly twice as slow when
the previous run used the other scanner. Their different access patterns leave
different useful metadata cached, so the preceding scanner is part of the
benchmark conditions.

This breaks the obvious alternating A/B experiment. Instead of repeatedly
measuring each scanner under its own settled conditions, it repeatedly measures
the penalty of switching access patterns. `benches/ab_pairs.ps1` alternates arms
and cannot measure this change reliably. It also aborts on the legitimate
file-count difference between the implementations.

Run each arm in consecutive blocks, discard the first run of each block, and
counterbalance block order in multiples of four, for example A, B, B, A.
Confirm the actual scanner using `scan_only`'s `scanner=mft|walker` output.

Correctness comparisons must also inspect attribution. Two errors can cancel
in a grand total. The `mft_diff` tool, available through `internal-tools`, dumps
a scan as a canonical sorted listing. Run it under different
`DISK_CLEANER_MFT` values and diff the listings.

On T:, the scanners agree on **617.7 GB to within 392 KB**, while **2,298 of
211,329 directories** differ in size, with the largest disagreement **11.2 GB**.
Total agreement is therefore weak evidence that directory-level results agree.

Warm repeated scans are also the wrong primary metric for a tool people open
occasionally. The first scan of a session matters most to that experience,
and it is **not measured here**. The reported results do not establish the
first-scan benefit.

## Known gaps

### Hardlink placement differs between scanners

The walker charges hard-linked bytes to the first name it meets in traversal
order; the MFT scanner chooses a path before tree construction. Each applies
its own accounting rule, so placement can disagree even when totals agree.
Aligning them requires retaining file identity and allocation through the
walker's finalisation, where a common ownership policy could be applied.

### Folder size is not recoverable space

No folder size is recoverable space. Deleting one link of a hard-linked file
frees nothing while another link remains. An honest presentation needs an
exclusive figure and a shared figure, computed over the whole deletion
selection rather than independently per folder. Selecting both linked folders
changes the answer in a way that adding their separate estimates cannot express.

### ACL parity stops at the root's direct children

Blocked directories deeper in an accessible root subtree are read through.
An elevated MFT scan therefore reports about **0.016% more** than the walker
on volumes containing such directories. Root visibility checks do not establish
recursive ACL parity.

### Record reuse can attach children to the wrong parent

Parent references retain the 48-bit record number and discard the 16-bit
sequence number. If a directory is deleted and its slot reused mid-scan,
children read at another moment can attach to the new occupant. There is no
snapshot, and the positional check at `0x2C` does not detect this identity change.

### Parse failures can remove whole subtrees

Parse errors raise the warning badge rather than triggering fallback. Dropping
one directory record can orphan all its descendants, silently omitting that
subtree from the displayed hierarchy apart from the warning. Error counts and
sampled error kinds help diagnose this, but do not recover the missing contents.

### Non-resident reparse attributes are not inspected

A junction whose target exceeds roughly **700 bytes** can have a non-resident
`$REPARSE_POINT` attribute. The scanner does not inspect that external value,
so it retains the junction as an ordinary directory instead of skipping it
as the walker does. The threshold is approximate, not a universal format limit.

### Hidden attributes can be stale

The NTFS hidden bit comes from the `$FILE_NAME` copy of the attributes, which
NTFS does not keep current. The tree also treats dot-prefixed names as hidden,
but that application convention does not correct stale NTFS attribute data.

### Probe implementations can diverge

The probe binaries carry a second implementation of raw reading and attribute
decoding. The implementations have already diverged once. A successful probe
does not by itself validate the production path; decoder changes need review
across both implementations, and production results need direct comparison.