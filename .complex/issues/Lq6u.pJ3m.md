Shell script (invoked by `seguro images build`) that produces base.qcow2.

Steps:
1. Download Alpine Linux virt ISO (minimal, virtio drivers included)
2. Boot it in QEMU with a cloud-init / answers file for unattended install
3. Install base packages: openssh, git, curl, nodejs, npm, python3, py3-pip, rust (via rustup or apk)
4. Create unprivileged user `agent` with home /home/agent
5. Write /etc/rc.local that:
   a. Reads /sys/firmware/qemu_fw_cfg/by_name/opt/seguro/authorized_key/raw → /home/agent/.ssh/authorized_keys
   b. Applies iptables rules for proxy redirect (HTTP/S → 10.0.2.100:3128, drop other outbound TCP, allow DNS)
6. Configure sshd: disable password auth, allow only agent user
7. --browser variant: also installs chromium chromium-chromedriver; sets PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH and PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD in /etc/environment
8. Compact and convert to qcow2 with qemu-img convert -c (compressed)

Output: base.qcow2 and base-browser.qcow2 in ~/.local/share/seguro/images/
Document expected sizes: ~200MB base, ~450MB browser.