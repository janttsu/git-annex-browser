#!/usr/bin/env bash
# Runs inside the screenshot VM as user "demo" (passwordless sudo).
# Installs packages, builds git-annex-browser from ~/src, creates the demo
# collection and starts an X session (Openbox) on the VM's display.
set -euo pipefail
cd "$HOME"

echo "== packages"
# Name resolution is not always ready right after first boot.
for _ in $(seq 60); do
    getent hosts archlinux.org >/dev/null && break
    sleep 1
done
if [ -n "${PACMAN_MIRROR:-}" ]; then
    echo "Server = $PACMAN_MIRROR" | sudo tee /etc/pacman.d/mirrorlist >/dev/null
fi
sudo pacman-key --init >/dev/null 2>&1 || true
sudo pacman-key --populate archlinux >/dev/null 2>&1 || true
for attempt in 1 2 3; do
    sudo pacman -Sy --noconfirm --needed archlinux-keyring >/dev/null && break
    [ "$attempt" = 3 ] && exit 1
    sleep 5
done
sudo pacman -Su --noconfirm >/dev/null
sudo pacman -S --noconfirm --needed \
    git git-annex rsync rust gcc \
    xorg-server xorg-xinit xorg-xrandr xorg-xsetroot xterm openbox xdotool \
    ttf-dejavu >/dev/null

echo "== build"
(cd src && cargo build --release --locked --quiet)
sudo install -m 0755 src/target/release/git-annex-browser /usr/local/bin/

echo "== demo data"
bash "$HOME/demo-data.sh" "$HOME/annex"
git-annex-browser --scan --quiet "$HOME/annex"

echo "== X session"
sudo tee /etc/X11/Xwrapper.config >/dev/null <<'EOF'
allowed_users=anybody
needs_root_rights=yes
EOF
cat >"$HOME/.Xresources" <<'EOF'
xterm*faceName: DejaVu Sans Mono
xterm*faceSize: 11
xterm*utf8: 1
xterm*locale: true
xterm*background: #1b1d2b
xterm*foreground: #c8ccd8
xterm*cursorColor: #c8ccd8
xterm*color4: #5f7fff
xterm*color12: #8aa2ff
xterm*internalBorder: 2
xterm*scrollBar: false
EOF
cat >"$HOME/.xinitrc" <<'EOF'
xrdb -merge "$HOME/.Xresources"
xsetroot -solid '#2e3440'
openbox &
exec sleep infinity
EOF
echo 'export LANG=C.UTF-8' >>"$HOME/.bashrc"
setsid -f xinit "$HOME/.xinitrc" -- :0 vt1 -nolisten tcp -nocursor -keeptty >"$HOME/xinit.log" 2>&1 </dev/null
for _ in $(seq 60); do
    DISPLAY=:0 xrandr >/dev/null 2>&1 && break
    sleep 1
done
DISPLAY=:0 xrandr | head -3
echo "== guest ready"
