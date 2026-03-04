#!/usr/bin/env bash
# build-image.sh — Build seguro base Alpine Linux images
#
# Usage: build-image.sh [--browser]
#
# Produces:
#   ~/.local/share/seguro/images/base.qcow2          (~200 MB)
#   ~/.local/share/seguro/images/base-browser.qcow2  (~450 MB, --browser)
#
# Requirements: qemu-system-x86_64, qemu-img, expect, wget or curl

set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────────────

ALPINE_VERSION="${ALPINE_VERSION:-3.21.3}"
ALPINE_ARCH="x86_64"
ALPINE_MAJOR_MINOR="${ALPINE_VERSION%.*}"
ALPINE_ISO="alpine-virt-${ALPINE_VERSION}-${ALPINE_ARCH}.iso"
ALPINE_ISO_URL="https://dl-cdn.alpinelinux.org/alpine/v${ALPINE_MAJOR_MINOR}/releases/${ALPINE_ARCH}/${ALPINE_ISO}"
ALPINE_ISO_SHA256_URL="${ALPINE_ISO_URL%.iso}.iso.sha256"

IMAGES_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/seguro/images"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/seguro"

BUILD_BROWSER=false
if [[ "${1:-}" == "--browser" ]]; then
    BUILD_BROWSER=true
fi

# Disk size for the working image before compaction
WORK_DISK_SIZE="3G"

# ── Helpers ───────────────────────────────────────────────────────────────────

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RESET='\033[0m'
log()  { echo -e "${GREEN}==>${RESET} $*"; }
warn() { echo -e "${YELLOW}warn:${RESET} $*" >&2; }
die()  { echo -e "${RED}error:${RESET} $*" >&2; exit 1; }

# ── Dependency checks ─────────────────────────────────────────────────────────

for cmd in qemu-system-x86_64 qemu-img expect; do
    command -v "$cmd" &>/dev/null || die "'$cmd' is required but not found on \$PATH"
done

# wget or curl for downloading
DOWNLOADER=""
if command -v wget &>/dev/null; then
    DOWNLOADER="wget"
elif command -v curl &>/dev/null; then
    DOWNLOADER="curl"
else
    die "wget or curl is required for downloading the Alpine ISO"
fi

# ── Setup ─────────────────────────────────────────────────────────────────────

mkdir -p "$IMAGES_DIR" "$CACHE_DIR"

TMP_DIR=$(mktemp -d)
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

# ── Download Alpine ISO ───────────────────────────────────────────────────────

ISO_PATH="$CACHE_DIR/$ALPINE_ISO"

if [[ ! -f "$ISO_PATH" ]]; then
    log "Downloading Alpine Linux ${ALPINE_VERSION} virt ISO..."
    if [[ "$DOWNLOADER" == "wget" ]]; then
        wget -q --show-progress -O "$ISO_PATH.tmp" "$ALPINE_ISO_URL"
    else
        curl -# -L -o "$ISO_PATH.tmp" "$ALPINE_ISO_URL"
    fi
    mv "$ISO_PATH.tmp" "$ISO_PATH"
    log "Download complete."
else
    log "Using cached ISO: $ISO_PATH"
fi

# ── Alpine answers file ───────────────────────────────────────────────────────
# Used with `setup-alpine -f` for unattended installation.

write_answers() {
    cat > "$TMP_DIR/answers" <<'ANSWERS'
KEYMAPOPTS="us us"
HOSTNAMEOPTS="-n alpine-seguro"
INTERFACESOPTS="auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
"
DNSOPTS="-d local -n 8.8.8.8 8.8.4.4"
TIMEZONEOPTS="-z UTC"
PROXYOPTS="none"
APKREPOSOPTS="-1"
SSHDOPTS="-c openssh"
NTPOPTS="-c none"
DISKOPTS="-m sys /dev/vda"
ANSWERS
}

# ── Phase 1: Install Alpine ───────────────────────────────────────────────────
# Boots the Alpine virt ISO, logs in as root, runs setup-alpine unattended.

run_install_phase() {
    local work_disk="$1"

    log "Phase 1: Installing Alpine Linux to disk..."
    qemu-img create -f qcow2 "$work_disk" "$WORK_DISK_SIZE"

    write_answers

    # Encode answers as base64 so we can pass them via the serial console
    local answers_b64
    answers_b64=$(base64 -w0 < "$TMP_DIR/answers")

    expect -f - <<EXPECT
set timeout 180
log_user 1

spawn qemu-system-x86_64 \
    -M q35 \
    -cpu host -enable-kvm \
    -m 1G -smp 2 \
    -drive file=${work_disk},format=qcow2,if=virtio \
    -cdrom ${ISO_PATH} \
    -boot d \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0 \
    -nographic -serial stdio \
    -no-reboot

# Wait for the login prompt (Alpine virt ISO boots fast)
expect {
    timeout { send_user "\nerror: timed out waiting for login prompt\n"; exit 1 }
    "localhost login:"
}
send "root\r"

expect {
    timeout { send_user "\nerror: timed out after login\n"; exit 1 }
    "# "
}

# Configure network (DHCP should auto-start on virt ISO, but be explicit)
send "ifconfig eth0 up && udhcpc -i eth0 -q 2>/dev/null; echo NETOK\r"
expect {
    timeout { send_user "\nerror: network setup timed out\n"; exit 1 }
    "NETOK"
}
expect "# "

# Decode answers file and run setup-alpine non-interactively
send "echo '${answers_b64}' | base64 -d > /tmp/answers\r"
expect "# "

send "setup-alpine -f /tmp/answers\r"

# setup-alpine will ask for disk format (ext4/btrfs/etc); accept default
expect {
    timeout { send_user "\nerror: setup-alpine timed out\n"; exit 1 }
    "Which disk(s) would you like to use?" { send "\r"; exp_continue }
    "How would you like to use it?" { send "sys\r"; exp_continue }
    "WARNING: Erase the above disk(s) and continue?" { send "y\r"; exp_continue }
    "Installation is complete"
}
expect "# "

send "poweroff\r"
expect eof
EXPECT

    log "Phase 1 complete."
}

# ── Phase 2: Configure packages and settings ──────────────────────────────────

run_configure_phase() {
    local work_disk="$1"
    local variant="$2"   # "base" or "browser"

    log "Phase 2: Configuring packages and settings ($variant)..."

    # Build the package list
    local pkgs="openssh git curl wget bash python3 py3-pip nodejs npm"
    if [[ "$variant" == "browser" ]]; then
        pkgs="$pkgs chromium chromium-chromedriver"
    fi

    # rc.local content: reads SSH key and env vars from fw_cfg, applies iptables
    local rclocal
    rclocal=$(cat <<'RCLOCAL'
#!/bin/sh
# Seguro guest startup script

FW_CFG=/sys/firmware/qemu_fw_cfg/by_name

# ── SSH authorized key ───────────────────────────────────────────────────────
AUTH_KEY_SRC="$FW_CFG/opt/seguro/authorized_key/raw"
if [ -f "$AUTH_KEY_SRC" ]; then
    install -d -m 700 -o agent -g agent /home/agent/.ssh
    install -m 600 -o agent -g agent "$AUTH_KEY_SRC" /home/agent/.ssh/authorized_keys
fi

# ── Environment variables ─────────────────────────────────────────────────────
ENV_DIR="$FW_CFG/opt/seguro/env"
if [ -d "$ENV_DIR" ]; then
    for f in "$ENV_DIR"/*/raw; do
        [ -f "$f" ] || continue
        # Extract variable name from path: .../opt/seguro/env/VAR_NAME/raw
        var=$(echo "$f" | sed 's|.*/opt/seguro/env/\([^/]*\)/raw|\1|')
        val=$(cat "$f")
        printf '%s=%s\n' "$var" "$val" >> /etc/environment
    done
fi

# ── Proxy iptables rules ──────────────────────────────────────────────────────
# Route all HTTP/HTTPS traffic through the seguro proxy at 10.0.2.100:3128.
# Drop all other outbound TCP and non-DNS UDP as defence-in-depth.
PROXY_ADDR="10.0.2.100"
PROXY_PORT="3128"

iptables -t nat -A OUTPUT ! -d "$PROXY_ADDR/24" -p tcp --dport 80  \
    -j DNAT --to-destination "${PROXY_ADDR}:${PROXY_PORT}"
iptables -t nat -A OUTPUT ! -d "$PROXY_ADDR/24" -p tcp --dport 443 \
    -j DNAT --to-destination "${PROXY_ADDR}:${PROXY_PORT}"

# Block non-proxy outbound TCP
iptables -A OUTPUT ! -d "$PROXY_ADDR/24" -p tcp -j DROP
# Allow DNS to SLIRP resolver only (10.0.2.3)
iptables -A OUTPUT -d 10.0.2.3 -p udp --dport 53 -j ACCEPT
iptables -A OUTPUT ! -d "$PROXY_ADDR/24" -p udp -j DROP

exit 0
RCLOCAL
)

    # sshd_config: disable password auth, allow only 'agent' user
    local sshdconfig
    sshdconfig=$(cat <<'SSHD'
PermitRootLogin no
PasswordAuthentication no
ChallengeResponseAuthentication no
AllowUsers agent
UsePAM no
PrintMotd no
Subsystem sftp /usr/lib/ssh/sftp-server
SSHD
)

    expect -f - <<EXPECT
set timeout 300
log_user 1

spawn qemu-system-x86_64 \
    -M q35 \
    -cpu host -enable-kvm \
    -m 1G -smp 2 \
    -drive file=${work_disk},format=qcow2,if=virtio \
    -netdev user,id=net0 \
    -device virtio-net-pci,netdev=net0 \
    -nographic -serial stdio \
    -no-reboot

expect {
    timeout { send_user "\nerror: timed out waiting for login\n"; exit 1 }
    "localhost login:"
}
send "root\r"
expect "# "

# Wait for networking
send "until ping -c1 -W2 dl-cdn.alpinelinux.org &>/dev/null; do sleep 2; done; echo NETREADY\r"
expect {
    timeout { send_user "\nerror: network not ready after 60s\n"; exit 1 }
    "NETREADY"
}
expect "# "

# Update and install packages
send "apk update && apk add --no-cache ${pkgs} iptables && echo PKGDONE\r"
expect {
    timeout { send_user "\nerror: package installation timed out\n"; exit 1 }
    "PKGDONE"
}
expect "# "

# Create unprivileged 'agent' user
send "adduser -D -s /bin/bash agent && echo USEROK\r"
expect {
    "USEROK" {}
    "already exists" { send_user "warn: user agent already exists\n" }
}
expect "# "

# Mount qemu_fw_cfg on boot (via fstab entry)
send {echo "none /sys/firmware/qemu_fw_cfg/by_name 9p trans=virtio,ro,nofail 0 0" >> /etc/fstab}
send "\r"
expect "# "

# Install rc.local
send "cat > /etc/rc.local << 'RCEOF'\r"
send "${rclocal}\r"
send "RCEOF\r"
expect "# "
send "chmod +x /etc/rc.local\r"
expect "# "
send "rc-update add local default\r"
expect "# "

# Harden sshd
send "cat > /etc/ssh/sshd_config << 'SSHDEOF'\r"
send "${sshdconfig}\r"
send "SSHDEOF\r"
expect "# "

# Browser variant: set Playwright env in /etc/environment
if {"${variant}" eq "browser"} {
    send {echo 'PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium-browser' >> /etc/environment}
    send "\r"
    expect "# "
    send {echo 'PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1' >> /etc/environment}
    send "\r"
    expect "# "
}

# Clean up APK cache to reduce image size
send "rm -rf /var/cache/apk/*\r"
expect "# "

send "poweroff\r"
expect eof
EXPECT

    log "Phase 2 complete."
}

# ── Build a single image variant ──────────────────────────────────────────────

build_variant() {
    local variant="$1"        # "base" or "browser"
    local output_name

    if [[ "$variant" == "base" ]]; then
        output_name="base.qcow2"
    else
        output_name="base-browser.qcow2"
    fi

    local output="$IMAGES_DIR/$output_name"
    local work_disk="$TMP_DIR/${variant}-work.qcow2"

    log "Building $output_name..."

    run_install_phase "$work_disk"
    run_configure_phase "$work_disk" "$variant"

    log "Compacting $output_name..."
    qemu-img convert -c -O qcow2 "$work_disk" "$output"

    local size
    size=$(du -sh "$output" | cut -f1)
    log "Built $output ($size)"
}

# ── Main ──────────────────────────────────────────────────────────────────────

log "seguro image builder — Alpine Linux ${ALPINE_VERSION}"
log "Output directory: $IMAGES_DIR"

build_variant "base"

if [[ "$BUILD_BROWSER" == "true" ]]; then
    build_variant "browser"
fi

log "Done."
