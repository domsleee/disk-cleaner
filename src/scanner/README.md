# Scanner

Two scanners produce the same `FileNode` tree on Windows.

- **Bulk directory walker** (`windows.rs`). Parallel `FILE_ID_BOTH_DIR_INFO`
  enumeration. Handles every case, and is the default.
- **Raw NTFS MFT reader** (`windows_ntfs.rs`). Reads `$MFT` sequentially and
  builds the tree from it. Used only for whole-volume scans, elevated, on NTFS
  fixed drives whose bus is on the whitelist. Anything else, including any
  error, falls back to the walker.

`DISK_CLEANER_MFT=auto|off|force` overrides the choice. `force` skips the
whitelist but not the eligibility checks. `scan_only` prints `scanner=mft|walker`
so a benchmark can confirm which one actually ran.

## Why the MFT path is gated

It is not always faster. Measured across four NTFS volumes on one machine:

| volume | bus | `$MFT` | files | walker | MFT | |
|---|---|---|---|---|---|---|
| C: | NVMe | 6.26 GB | 5.83M | 7,837 ms | 5,887 ms | 25% faster |
| T: | NVMe | 1.79 GB | 1.70M | 2,501 ms | 2,292 ms | 8% faster |
| D: | SATA | 3.10 GB | 2.74M | 3,038 ms | 9,435 ms | **3.1x slower** |
| E: | SATA | 760 MB | 479k | 388 ms | 2,218 ms | **5.7x slower** |

The MFT path is parse-bound, not disk-bound: it achieves 0.78-1.06 GB/s on NVMe,
far below what those disks deliver, which works out at roughly 1.0-1.4 us/file
against the walker's 0.8-1.5. So this is a near-tie decided by device latency.
The upside is capped near 25% and a faster disk does not widen it; the downside
on the wrong device is several times slower.

`bus_favours_mft` is therefore a **whitelist of buses measured to win**, not a
cost model. Volume shape is deliberately absent: counting in-use records from
`$MFT`'s `$BITMAP` moves the predicted ratios to 0.66/0.65/2.39/3.36, which
changes none of those four decisions, because the bus gap (3.4x) dwarfs the
shape term (at most 1.5x). It would start to matter on a high-free-fraction NVMe
volume. Build and measure that case before adding it.

Measuring achieved throughput instead was tried and reverted: reading a prefix
of `$MFT` to decide evicts the metadata cache the walker then falls back to,
which made D:, E: and T: *worse* than having no gate at all. The prediction has
to be free.

## Benchmarking this

Two things make the obvious approach wrong.

**The scanners evict each other's caches.** A run is roughly twice as slow when
the previous run used the other scanner. So `benches/ab_pairs.ps1`, which
alternates arms, cannot measure this change; it also aborts on the legitimate
file-count difference between the two. Run each arm in consecutive blocks
instead, discard the first run of each block, and counterbalance the block order
in multiples of four.

**Totals hide attribution errors.** Two errors cancel. `mft_diff` (internal-tools)
dumps a scan as a canonical sorted listing; run it twice under different
`DISK_CLEANER_MFT` values and diff. On T: the two scanners agree on 617.7 GB to
within 392 KB while 2,298 of 211,329 directories differ in size, the largest by
11.2 GB.

Warm numbers are also the wrong metric for a tool people open occasionally. The
first scan of a session is the case that matters, and it is not measured here.

## Semantics

**Sizes are allocated, not logical**, matching the walker (#99). Sparse and
compressed `$DATA` attributes span their whole VCN range, so `AllocatedSize` at
offset 0x28 counts holes as written; the physical bytes are in
`TotalAllocatedSize` at 0x40, which is why the parser requires `attr_len >= 0x48`.

**Hard links** are counted once. Which name is charged is chosen before the
parallel tree build, as the lexicographically smallest volume-relative path among
names the tree will reach. Without that the racing tasks decide, and directory
sizes move between scans of an unchanged drive (measured: 636 directories).

**Record numbers are positional** — `base + index` across runs and chunks. A
short read or bad runlist would renumber the rest of the volume silently, so each
record's own number at header offset 0x2C is checked against its position.

**Root visibility** mirrors a Win32 listing: metafiles are hidden and root
directories that cannot be opened stay empty, with `SeBackupPrivilege` suspended
so the check reflects an ordinary listing.

## Known gaps

- The walker charges hard-linked bytes to the first name it meets in traversal
  order, so the two scanners disagree about placement even though each is
  internally consistent. Fixing this means keeping file identity and allocation
  alive through the walker's finalisation.
- **No folder size is recoverable space.** Deleting one link of a hard-linked
  file frees nothing. The honest presentation is an exclusive figure plus a
  shared figure, computed over the whole deletion selection rather than per
  folder.
- ACL parity holds only for the root's direct children. Blocked directories
  deeper down are read through, so an elevated MFT scan reports about 0.016% more
  than the walker on volumes that have them.
- Parent references keep the 48-bit record number and discard the 16-bit sequence
  number, so a record deleted and reused mid-scan can attach a subtree to the
  wrong parent. There is no snapshot.
- Parse errors raise the warning badge rather than falling back, so one dropped
  directory record silently omits its subtree.
- Non-resident `$REPARSE_POINT` attributes are not inspected, so a junction whose
  target exceeds roughly 700 bytes is kept as an ordinary directory instead of
  being skipped as the walker skips it.
- `hidden` comes from the `$FILE_NAME` copy of the attributes, which NTFS does
  not keep current.
- The probe binaries carry a second implementation of the raw read and attribute
  decode. The two have already diverged once.
