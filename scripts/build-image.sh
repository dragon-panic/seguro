#!/usr/bin/env bash
# build-image.sh — Build seguro base Ubuntu 24.04 images
#
# Usage: build-image.sh [--browser]
#
# Produces:
#   ~/.local/share/seguro/images/base.qcow2          (~500 MB)
#   ~/.local/share/seguro/images/base-browser.qcow2  (~900 MB, --browser)
#
# Requirements: qemu-system-x86_64, qemu-img, wget or curl, mkfs.vfat (dosfstools), mcopy (mtools)

set -euo pipefail

# ── Configuration ────────────────────────────────────────────────────────────

UBUNTU_VERSION="24.04"
UBUNTU_CODENAME="noble"
UBUNTU_IMG="ubuntu-${UBUNTU_VERSION}-minimal-cloudimg-amd64.img"
UBUNTU_IMG_URL="https://cloud-images.ubuntu.com/minimal/releases/${UBUNTU_CODENAME}/release/${UBUNTU_IMG}"

IMAGES_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/seguro/images"
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/seguro"

BUILD_BROWSER=false
if [[ "${1:-}" == "--browser" ]]; then
    BUILD_BROWSER=true
fi

# ── Helpers ───────────────────────────────────────────────────────────────────

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RESET='\033[0m'
log()  { echo -e "${GREEN}==>${RESET} $*"; }
warn() { echo -e "${YELLOW}warn:${RESET} $*" >&2; }
die()  { echo -e "${RED}error:${RESET} $*" >&2; exit 1; }

# ── Dependency checks ─────────────────────────────────────────────────────────

for cmd in qemu-system-x86_64 qemu-img mkfs.vfat mcopy; do
    command -v "$cmd" &>/dev/null || die \
        "'$cmd' is required but not found on \$PATH.
  Install dosfstools (mkfs.vfat) and mtools (mcopy):
    Arch:   sudo pacman -S dosfstools mtools
    Debian: sudo apt install dosfstools mtools"
done

DOWNLOADER=""
if command -v wget &>/dev/null; then
    DOWNLOADER="wget"
elif command -v curl &>/dev/null; then
    DOWNLOADER="curl"
else
    die "wget or curl is required for downloading the Ubuntu image"
fi

# ── Setup ─────────────────────────────────────────────────────────────────────

mkdir -p "$IMAGES_DIR" "$CACHE_DIR"

TMP_DIR=$(mktemp -d)
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

# ── Download Ubuntu Minimal cloud image ───────────────────────────────────────

BASE_IMG="$CACHE_DIR/$UBUNTU_IMG"

if [[ ! -f "$BASE_IMG" ]]; then
    log "Downloading Ubuntu ${UBUNTU_VERSION} (${UBUNTU_CODENAME}) minimal cloud image..."
    if [[ "$DOWNLOADER" == "wget" ]]; then
        wget -q --show-progress -O "$BASE_IMG.tmp" "$UBUNTU_IMG_URL"
    else
        curl -# -L -o "$BASE_IMG.tmp" "$UBUNTU_IMG_URL"
    fi
    mv "$BASE_IMG.tmp" "$BASE_IMG"
    log "Download complete."
else
    log "Using cached image: $BASE_IMG"
fi

# ── Create NoCloud seed disk ──────────────────────────────────────────────────
# Writes meta-data and user-data into a 512 KB FAT12 image labelled "cidata".

make_seed() {
    local seed_path="$1"
    local user_data_path="$2"
    local meta_data_path="$3"

    truncate -s 512k "$seed_path"
    mkfs.vfat -F 12 -n cidata "$seed_path" >/dev/null
    mcopy -i "$seed_path" "$meta_data_path" "::meta-data"
    mcopy -i "$seed_path" "$user_data_path" "::user-data"
}

# ── Build a single image variant ──────────────────────────────────────────────

build_variant() {
    local variant="$1"   # "base" or "browser"
    local output_name

    if [[ "$variant" == "base" ]]; then
        output_name="base.qcow2"
    else
        output_name="base-browser.qcow2"
    fi

    local output="$IMAGES_DIR/$output_name"
    local work_disk="$TMP_DIR/${variant}-work.qcow2"

    log "Building $output_name..."

    # Create a 10 G copy-on-write overlay over the downloaded cloud image.
    # package installs need headroom; qemu-img convert will compact at the end.
    qemu-img create -f qcow2 -b "$BASE_IMG" -F qcow2 "$work_disk" 10G

    # ── meta-data ────────────────────────────────────────────────────────────

    cat > "$TMP_DIR/meta-data" <<'META'
instance-id: seguro-build
local-hostname: seguro-build
META

    # ── user-data ────────────────────────────────────────────────────────────
    # cloud-init runs once per instance-id, installs packages, creates the
    # agent user, then powers the VM off.  The power_state module triggers
    # after cloud-final completes, so QEMU exits cleanly without -no-reboot.

    local extra_pkgs=""
    if [[ "$variant" == "browser" ]]; then
        extra_pkgs="  - chromium-browser"
    fi

    {
        cat <<'UDHEAD'
#cloud-config
package_update: true
package_upgrade: false
packages:
  - git
  - curl
  - wget
  - python3
  - python3-pip
  - nodejs
  - npm
  - iptables
UDHEAD
        if [[ -n "$extra_pkgs" ]]; then
            echo "$extra_pkgs"
        fi
        cat <<'UDTAIL'
users:
  - name: agent
    shell: /bin/bash
    lock_passwd: true
    no_create_home: false

ssh_pwauth: false

write_files:
  - path: /etc/fstab
    append: true
    content: |
      workspace  /home/agent/workspace  virtiofs  defaults,nofail  0  0

runcmd:
  - mkdir -p /home/agent/workspace
  - chown agent:agent /home/agent/workspace

power_state:
  mode: poweroff
  delay: now
  timeout: 30
UDTAIL
        if [[ "$variant" == "browser" ]]; then
            cat <<'UDBROWSER'
  - path: /etc/environment.d/99-playwright.conf
    content: |
      PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH=/usr/bin/chromium-browser
      PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
UDBROWSER
        fi
    } > "$TMP_DIR/user-data"

    # ── Seed disk ─────────────────────────────────────────────────────────────

    local seed="$TMP_DIR/${variant}-seed.img"
    make_seed "$seed" "$TMP_DIR/user-data" "$TMP_DIR/meta-data"

    # ── Boot: cloud-init installs packages, then powers off ───────────────────

    log "Running cloud-init setup (installs packages — takes a few minutes)..."

    qemu-system-x86_64 \
        -M q35 \
        -cpu host -enable-kvm \
        -m 2G -smp 2 \
        -drive "file=${work_disk},format=qcow2,if=virtio" \
        -drive "file=${seed},format=raw,if=virtio,readonly=on" \
        -netdev user,id=net0 \
        -device virtio-net-pci,netdev=net0 \
        -display none -serial null \
        -no-reboot

    log "Compacting $output_name..."
    qemu-img convert -c -O qcow2 "$work_disk" "$output"

    local size
    size=$(du -sh "$output" | cut -f1)
    log "Built $output ($size)"
}

# ── Main ──────────────────────────────────────────────────────────────────────

log "seguro image builder — Ubuntu ${UBUNTU_VERSION} (${UBUNTU_CODENAME})"
log "Output directory: $IMAGES_DIR"

build_variant "base"

if [[ "$BUILD_BROWSER" == "true" ]]; then
    build_variant "browser"
fi

log "Done."
