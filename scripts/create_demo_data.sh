#!/bin/bash
# Create a demo directory structure for screenshots.
# Includes hidden folders, file grouping triggers, and varied file types.
# Uses real (non-sparse) files so the scanner reports correct sizes on unix.
set -euo pipefail

DIR="${1:-/tmp/disk-cleaner-demo}"
rm -rf "$DIR"
mkdir -p "$DIR"

# Helper: create a real file of a given size in KiB
mkfile() {
  local path="$1" size_kb="$2"
  mkdir -p "$(dirname "$path")"
  dd if=/dev/zero of="$path" bs=1024 count="$size_kb" 2>/dev/null
}

# ── Top-level directories ──

# Videos (large, few files)
mkfile "$DIR/Videos/Tutorials/rust-course.mp4"       50000   # 50 MB
mkfile "$DIR/Videos/Tutorials/react-basics.mp4"      30000   # 30 MB

# Projects (nested, varied sizes — triggers file grouping in rest-api/src)
mkfile "$DIR/Projects/rest-api/node_modules/.cache/babel/cache.json" 12000
mkfile "$DIR/Projects/rest-api/node_modules/.cache/eslint/cache.bin"  8000
mkfile "$DIR/Projects/rest-api/dist/bundle.js"       13000
mkfile "$DIR/Projects/rest-api/src/index.ts"             5
mkfile "$DIR/Projects/rest-api/src/routes.ts"            3
mkfile "$DIR/Projects/rest-api/src/middleware.ts"         3
mkfile "$DIR/Projects/rest-api/package.json"             2
mkfile "$DIR/Projects/web-app/dist/app.js"           10000
mkfile "$DIR/Projects/web-app/.next/cache/webpack.pack" 9000
mkfile "$DIR/Projects/web-app/src/App.tsx"               4
mkfile "$DIR/Projects/web-app/src/index.tsx"             1
mkfile "$DIR/Projects/web-app/src/styles.css"            6
mkfile "$DIR/Projects/web-app/package.json"              2

# Downloads (many loose files → triggers file grouping)
mkfile "$DIR/Downloads/installer.dmg"                25000
mkfile "$DIR/Downloads/update.pkg"                   18000
mkfile "$DIR/Downloads/archive.zip"                  12000
mkfile "$DIR/Downloads/photo-export.tar.gz"           8500
mkfile "$DIR/Downloads/presentation.pptx"             4500
mkfile "$DIR/Downloads/report-2024.pdf"               1200
mkfile "$DIR/Downloads/notes.txt"                       50
mkfile "$DIR/Downloads/budget.xlsx"                    300

# Documents (moderate)
mkfile "$DIR/Documents/Presentations/q4-review.pptx"  4000
mkfile "$DIR/Documents/Presentations/roadmap.pptx"    3500
mkfile "$DIR/Documents/Reports/annual-2024.pdf"       1500
mkfile "$DIR/Documents/Reports/monthly-jan.pdf"        800

# Photos
mkfile "$DIR/Photos/Vacation-2024/IMG_0001.jpg"        800
mkfile "$DIR/Photos/Vacation-2024/IMG_0002.jpg"        750
mkfile "$DIR/Photos/Vacation-2024/IMG_0003.jpg"        900
mkfile "$DIR/Photos/Screenshots/screen-01.png"         200
mkfile "$DIR/Photos/Screenshots/screen-02.png"         180

# Music
mkfile "$DIR/Music/Albums/playlist-export.m4a"        2500

# ── Hidden directories (the key feature to showcase) ──
mkfile "$DIR/.cache/pip/wheels/numpy.whl"            15000
mkfile "$DIR/.cache/pip/wheels/pandas.whl"            9000
mkfile "$DIR/.cache/homebrew/downloads/gcc.tar.gz"   20000
mkfile "$DIR/.npm/_cacache/content/sha512-a"          8000
mkfile "$DIR/.npm/_cacache/content/sha512-b"          6000
mkfile "$DIR/.config/Code/Cache/data_0"               4000
mkfile "$DIR/.config/Code/Cache/data_1"               3500

# Library (macOS-style)
mkfile "$DIR/Library/Caches/com.apple.Safari/data.db"  6000
mkfile "$DIR/Library/Caches/com.spotify.client/cache"  4500

echo "Demo data created at $DIR"
echo "Total: $(du -sh "$DIR" | cut -f1)"
