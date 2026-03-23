# TODOS

## Windows: absolute-path suggestions (%TEMP%, Windows Update cache)
**Status:** Deferred (post-v1)
**Context:** The suggestion engine matches directory names (node_modules, target, etc.) which works cross-platform. Windows has platform-specific cleanup targets resolved via env vars (`%TEMP%` → `C:\Users\<user>\AppData\Local\Temp`). Requires a separate code path using `std::env::var("TEMP")` to resolve paths, then matching scanned paths against them. These directories can be very large (multi-GB).
**Depends on:** Cross-platform support and suggestions threshold shipping first.

## Release binary packaging (.deb, .msi, .dmg)
**Status:** Deferred (post-v1)
**Context:** Now that the app builds on all three platforms via CI, the next step is producing installable release binaries. macOS needs a `.dmg` or `.app` bundle (via `cargo-bundle` or `create-dmg`), Windows needs an `.msi` installer (via WiX or `cargo-wix`), and Linux needs a `.deb` package (via `cargo-deb`). These can be automated as a GitHub Actions release workflow triggered by version tags. Without this, users must build from source.
**Depends on:** Cross-platform CI passing on all three platforms.

## Linux: improve volume detection beyond fstype denylist
**Status:** Deferred (post-v1)
**Context:** The current `/proc/mounts` parser uses a hardcoded fstype denylist to filter virtual filesystems. This hides valid user data on bind mounts, btrfs subvolumes, and FUSE-backed storage. Fix: also check if the mount's device path starts with `/dev/` as a positive signal, and show FUSE mounts that have a real backing path. Users can always open any directory via the file dialog as an escape hatch, so this is a convenience improvement, not a hard failure.
**Depends on:** Linux volume listing shipping first.
