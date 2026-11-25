#!/usr/bin/env bash
# shellcheck disable=SC2034
# J.A.I.OS's install stick: an archiso profile, from Arch's releng. Built by the `J.A.I.OS`
# workflow (.github/workflows/jaios.yml): `mkarchiso -v -w <work> -o <out> os/iso`, after
# %JAIOS_REPO% in pacman.conf is set to the folder of the freshly built [jaios] packages and
# syslinux/ has its head and picture from `jai --os-look` (jaios-settings: /usr/share/jaios/iso).
#
# shortcut: the stick's own boot loaders are not signed, so Secure Boot is switched off for the
# install (the installed system signs its own with sbctl); sign them once there is a key to sign
# with that a new PC trusts.

iso_name="jaios"
iso_label="JAIOS_$(date --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y%m)"
iso_publisher="J.A.I.OS <https://github.com/JayKaey/JAI>"
iso_application="J.A.I.OS install stick"
iso_version="$(date --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" +%Y.%m.%d)"
install_dir="arch"
buildmodes=('iso')
bootmodes=('bios.syslinux'
           'uefi.systemd-boot')
arch="x86_64"
pacman_conf="pacman.conf"
airootfs_image_type="squashfs"
# (As small as xz makes it: the stick stays under GitHub's 2 GiB for one file.)
airootfs_image_tool_options=('-comp' 'xz' '-Xbcj' 'x86' '-b' '1M' '-Xdict-size' '1M')
file_permissions=(
  ["/etc/sudoers.d"]="0:0:750"
  ["/etc/sudoers.d/liveuser"]="0:0:440"
  ["/root"]="0:0:750"
)
