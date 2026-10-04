#!/usr/bin/env bash
# Build a demo git-annex collection for README screenshots. Every state is made
# with ordinary git / git-annex commands, so what the browser shows is real.
#
#   ~/annex/<repo>          the "laptop" clones (this is the scan root)
#   /media/usb-archive/...  a USB drive with clones of every repo (trusted)
#   /srv/nas/...            a NAS with clones of every repo (group backup)
#   /srv/offsite/...        rsync special remote "offsite"
#   /srv/glacier/...        rsync special remote "glacier", marked untrusted
#   old-laptop              a retired clone, marked dead (hidden by the browser)
#
# Usage: demo-data.sh [ROOT]   (default ROOT=$HOME/annex; needs sudo for /media and /srv)
set -euo pipefail
RANDOM=4242 # same file sizes on every run

ROOT=${1:-$HOME/annex}
# Drive locations can be moved (e.g. for a test run without sudo).
USB=${USB_DIR:-/media/usb-archive}
NAS=${NAS_DIR:-/srv/nas}
OFFSITE=${OFFSITE_DIR:-/srv/offsite}
GLACIER=${GLACIER_DIR:-/srv/glacier}
OLD=${OLD_DIR:-/srv/old-laptop}

export GIT_AUTHOR_DATE="2026-09-01T10:00:00" GIT_COMMITTER_DATE="2026-09-01T10:00:00"
git config --global user.name "Demo User" >/dev/null
git config --global user.email "demo@example.org" >/dev/null
git config --global init.defaultBranch main >/dev/null

for d in "$USB" "$NAS" "$OFFSITE" "$GLACIER" "$OLD"; do
    if ! mkdir -p "$d" 2>/dev/null; then
        sudo mkdir -p "$d"
        sudo chown "$(id -u):$(id -g)" "$d"
    fi
done
mkdir -p "$ROOT"

# mkfile PATH SIZE_KIB — random content so every file is a distinct key.
mkfile() {
    mkdir -p "$(dirname "$1")"
    head -c "$(($2 * 1024))" /dev/urandom >"$1"
}

# new_repo NAME NUMCOPIES — laptop repo plus usb-archive and nas clones as remotes.
new_repo() {
    local name=$1 numcopies=$2
    local r="$ROOT/$name"
    mkdir -p "$r"
    cd "$r"
    git init -q
    git annex init -q "laptop"
    git annex numcopies "$numcopies" >/dev/null 2>&1
}

# attach NAME — create drive clones and special remotes once content is committed.
attach() {
    local name=$1
    local r="$ROOT/$name"
    cd "$r"
    for drive in usb-archive nas; do
        local dir
        if [ "$drive" = usb-archive ]; then dir="$USB/$name"; else dir="$NAS/$name"; fi
        git clone -q "$r" "$dir"
        (cd "$dir" && git annex init -q "$drive")
        git remote add "$drive" "$dir"
    done
    {
        git annex initremote offsite type=rsync rsyncurl="$OFFSITE/$name" encryption=none
        git annex initremote glacier type=rsync rsyncurl="$GLACIER/$name" encryption=none
        git annex untrust glacier
        git annex group nas backup
        git annex wanted nas standard
        git annex group usb-archive archive
        git annex wanted usb-archive standard
        git annex group offsite backup
        git annex trust --force usb-archive
    } >/dev/null 2>&1
}

sync_all() {
    git annex sync -q --no-content >/dev/null 2>&1 || git annex sync -q --no-content
}

# fsck_everywhere NAME — records fsck times (activity.log) for the laptop and both drives.
fsck_everywhere() {
    local name=$1
    (cd "$ROOT/$name" && git annex fsck -q --fast >/dev/null 2>&1 || true)
    (cd "$USB/$name" && git annex fsck -q --fast >/dev/null 2>&1 || true)
    (cd "$NAS/$name" && git annex fsck -q --fast >/dev/null 2>&1 || true)
    cd "$ROOT/$name"
    sync_all
}

### photos: numcopies 2, a few recent shots not backed up yet (at risk)
new_repo photos 2
for y in 2023 2024 2025; do
    for i in $(seq -f %02g 1 8); do
        mkfile "$y/IMG_${y}${i}.jpg" $((2400 + RANDOM % 3200))
    done
done
for i in $(seq -f %02g 1 4); do mkfile "2025/raw/DSC0${i}.NEF" $((18000 + RANDOM % 6000)); done
for i in $(seq -f %02g 1 5); do mkfile "2026/IMG_2026${i}.jpg" $((2800 + RANDOM % 2000)); done
git annex add -q . && git commit -q -m "photos"
attach photos
git annex copy -q --to usb-archive 2023 2024 2025
git annex copy -q --to nas 2023 2024
git annex copy -q --to glacier 2023 2024 2025 2026/IMG_202601.jpg 2026/IMG_202602.jpg
git annex copy -q --to offsite 2025/raw
sync_all
fsck_everywhere photos

### videos: numcopies 2, big files mostly dropped from the laptop (missing here)
new_repo videos 2
mkfile "2024/summer/lake.mp4" 61440
mkfile "2024/summer/sauna.mp4" 38912
mkfile "2024/wedding/ceremony.mp4" 90112
mkfile "2024/wedding/speeches.mp4" 51200
mkfile "2025/roadtrip/aurora.mov" 71680
mkfile "2025/roadtrip/coastline.mov" 45056
mkfile "2026/drone/archipelago.mp4" 66560
git annex add -q . && git commit -q -m "videos"
attach videos
# Spread copies so getting everything back needs more than one drive.
git annex copy -q --to usb-archive 2024/wedding 2025 2026
git annex copy -q --to nas 2024
git annex copy -q --to offsite 2024/summer 2025
sync_all
git annex drop -q 2024 2025
fsck_everywhere videos

### music: numcopies 1; usb-archive left semitrusted here, so it differs from the other repos
new_repo music 1
for a in "Aurora Borealis" "Harbor Lights" "Midnight Sun" "Northern Lights Trio"; do
    for t in 1 2 3 4 5 6; do
        mkfile "$a/0$t - track $t.flac" $((5200 + RANDOM % 4000))
    done
done
git annex add -q . && git commit -q -m "music"
attach music
git annex semitrust usb-archive >/dev/null 2>&1
git annex copy -q --to usb-archive "Aurora Borealis" "Harbor Lights"
git annex copy -q --to nas .
sync_all
fsck_everywhere music

### documents: numcopies 2, an edited file leaves an old version on the NAS
new_repo documents 2
for f in taxes/2023.pdf taxes/2024.pdf taxes/2025.pdf contracts/apartment.pdf \
    contracts/car-lease.pdf manuals/dishwasher.pdf manuals/heat-pump.pdf \
    scans/passport.png scans/diploma.png notes/recipes.odt notes/garden-plan.odt; do
    mkfile "$f" $((80 + RANDOM % 900))
done
mkfile "archive/old-projects.tar.gz" 24576
git annex add -q . && git commit -q -m "documents"
attach documents
git annex copy -q --to nas .
git annex copy -q --to usb-archive taxes contracts scans
git annex copy -q --to offsite taxes contracts
sync_all
git annex unlock -q notes/garden-plan.odt
mkfile "notes/garden-plan.odt" 610
git annex add -q notes/garden-plan.odt && git commit -q -m "update garden plan"
git annex copy -q --to usb-archive notes/garden-plan.odt
sync_all
fsck_everywhere documents

### backups: numcopies 3, the retired laptop still holds copies but is marked dead
new_repo backups 3
mkfile "laptop/home-2026-06.tar.zst" 81920
mkfile "laptop/home-2026-09.tar.zst" 86016
mkfile "phone/phone-2026-08.tar" 40960
mkfile "router/config-2026.tgz" 64
git annex add -q . && git commit -q -m "backups"
attach backups
git clone -q "$ROOT/backups" "$OLD/backups"
(cd "$OLD/backups" && git annex init -q "old-laptop")
git remote add old-laptop "$OLD/backups"
git annex copy -q --to old-laptop laptop
git annex copy -q --to nas .
git annex copy -q --to usb-archive laptop phone
git annex copy -q --to offsite laptop/home-2026-09.tar.zst
sync_all
git annex dead old-laptop >/dev/null 2>&1
git remote remove old-laptop
fsck_everywhere backups

echo "demo collection ready under $ROOT"
