#!/usr/bin/env bash
# Regenerate the README screenshots inside a throwaway Arch Linux VM.
#
# The VM builds git-annex-browser from the committed source (git archive HEAD),
# creates a demo collection with real git-annex commands (demo-data.sh), runs the
# TUI in xterm under Openbox, and every image is the VM's own screen captured by
# QEMU's `screendump`. Nothing is drawn or edited on the host.
#
# Requires: qemu-system-x86_64 with KVM, qemu-img, xorriso, ssh, curl, python3.
# Usage:    scripts/screenshots/run.sh [OUT_DIR]      (default: assets/)
#           KEEP_VM=1 keeps the VM running and its work directory afterwards.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
out=${1:-$repo/assets}
cache_dir=${XDG_CACHE_HOME:-$HOME/.cache}/git-annex-browser-vm
official=https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2
# IMAGE_URL: any Arch mirror's images/latest/…qcow2; the checksum always comes from the official site.
image_url=${IMAGE_URL:-$official}
# PACMAN_MIRROR: e.g. https://mirror.srv.fail/archlinux/$repo/os/$arch (default: image's mirrorlist)
pacman_mirror=${PACMAN_MIRROR:-}
base=$cache_dir/Arch-Linux-x86_64-cloudimg.qcow2
port=${SSH_PORT:-2229}
width=1640
height=920
work=$(mktemp -d "${TMPDIR:-/tmp}/gab-shots.XXXXXX")

log() { printf '\n== %s\n' "$*" >&2; }

monitor() {
    python3 - "$work/monitor.sock" "$1" <<'EOF'
import socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.settimeout(5)
s.recv(4096)  # banner
s.sendall((sys.argv[2] + "\n").encode())
time.sleep(1)
try:
    s.recv(65536)
except socket.timeout:
    pass
EOF
}

cleanup() {
    if [ "${KEEP_VM:-0}" = 1 ]; then
        log "VM kept: ssh -i $work/key -p $port demo@127.0.0.1  (work dir $work)"
        return
    fi
    [ -S "$work/monitor.sock" ] && monitor quit 2>/dev/null || true
    rm -rf "$work"
}
trap cleanup EXIT

if [ ! -f "$base" ]; then
    log "downloading Arch Linux cloud image"
    mkdir -p "$cache_dir"
    curl -fsSL -o "$base.SHA256" "$official.SHA256"
    curl -fL --retry 3 -o "$base.part" "$image_url"
    [ "$(sha256sum "$base.part" | cut -d' ' -f1)" = "$(cut -d' ' -f1 "$base.SHA256")" ] || {
        echo "checksum mismatch" >&2
        exit 1
    }
    mv "$base.part" "$base"
fi

log "preparing VM disk and cloud-init seed"
# Overlay: the cached base image is never modified.
qemu-img create -q -f qcow2 -F qcow2 -b "$base" "$work/disk.qcow2" 24G
ssh-keygen -q -t ed25519 -N '' -C gab-screenshots -f "$work/key"
cat >"$work/user-data" <<EOF
#cloud-config
hostname: laptop
users:
  - name: demo
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - $(cat "$work/key.pub")
EOF
printf 'instance-id: gab-screenshots\nlocal-hostname: laptop\n' >"$work/meta-data"
xorriso -as mkisofs -quiet -o "$work/seed.iso" -V cidata -J -r "$work/user-data" "$work/meta-data"

log "booting VM (${width}x${height} display)"
qemu-system-x86_64 \
    -enable-kvm -cpu host -smp "$(nproc --ignore=2)" -m 6G \
    -drive file="$work/disk.qcow2",if=virtio \
    -drive file="$work/seed.iso",media=cdrom \
    -netdev user,id=net0,ipv6=off,hostfwd=tcp:127.0.0.1:"$port"-:22 \
    -device virtio-net-pci,netdev=net0 \
    -device VGA,edid=on,xres="$width",yres="$height" \
    -display none \
    -monitor unix:"$work/monitor.sock",server,nowait \
    -pidfile "$work/qemu.pid" -daemonize

ssh_opts=(-i "$work/key" -p "$port" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=3)
vm() { ssh "${ssh_opts[@]}" demo@127.0.0.1 "$@"; }

for _ in $(seq 120); do
    vm true 2>/dev/null && break
    sleep 2
done
vm 'cloud-init status --wait >/dev/null 2>&1 || true'

log "copying source ($(git -C "$repo" rev-parse --short HEAD)) and scripts"
git -C "$repo" archive --format=tar HEAD | vm 'mkdir -p src && tar -x -C src'
scp -q -P "$port" -i "$work/key" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR \
    "$here/demo-data.sh" "$here/guest-setup.sh" demo@127.0.0.1:
vm "PACMAN_MIRROR=$(printf %q "$pacman_mirror") bash guest-setup.sh"

### Scenes ---------------------------------------------------------------

x() { vm "export DISPLAY=:0 LANG=C.UTF-8; $*"; }
keys() {
    x xdotool key --delay 120 "$@"
    sleep 1
}
typ() { x "xdotool type --delay 60 -- $(printf %q "$1")"; }
# Select the first row whose label contains TEXT: filter, jump to first match, clear filter.
select_row() {
    keys slash
    typ "$1"
    keys Return g Escape
}
to_root() { keys Escape Escape Escape Escape; }
shot() {
    sleep 1.5
    monitor "screendump $work/$1.png -f png"
    install -m 0644 "$work/$1.png" "$out/$1.png"
    log "captured $out/$1.png"
}

log "capturing screenshots"
mkdir -p "$out"
x "setsid -f xterm -T git-annex-browser -maximized -e git-annex-browser /home/demo/annex >/dev/null 2>&1"
sleep 6

shot main-view

select_row music
keys Right
select_row "drives /"
keys Right
select_row usb-archive
shot drives-view

to_root
select_row videos
keys Right
select_row "disk usage"
keys Right
# Into 2024/wedding: content was dropped from the laptop, so rows are grey but keep their size.
select_row "2024/"
keys Right
select_row "wedding/"
keys Right
shot usage-view

to_root
select_row videos
keys Right
select_row "missing here"
shot missing-here-view

to_root
select_row photos
keys Right
select_row "at risk"
keys Right
keys j
shot files-view

keys q
vm 'sudo poweroff' >/dev/null 2>&1 || true
log "done: $(ls "$out"/*-view.png | tr '\n' ' ')"
