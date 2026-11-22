# The Tileception ISO

The ISO is Ubuntu's own installer, set up to install Tileception with a set of
apps. Use it to start a computer fresh. To add Tileception to a system you
already have, use the [packages](installation.md#packages) instead.

Download it from [tileception.space/download](https://tileception.space/download/).
There is one per supported Ubuntu release, for PCs (amd64) and ARM computers
(arm64). The ISOs hold none of Tileception
itself: the installer downloads the current release, so the computer needs
internet while installing.

## Write it to a USB stick

Use a stick of 8 GB or more. With [Ventoy](https://www.ventoy.net/), set up
the stick once (this erases it) and copy the ISO onto it like any other file.
On a Mac, or when Ventoy does not work for you, write the ISO with any USB
writing tool, such as [balenaEtcher](https://etcher.balena.io/).

To check the download first:

```sh
curl -fsSLO https://tileception.space/download/SHA256SUMS
curl -fsSLO https://tileception.space/download/SHA256SUMS.gpg
curl -fsSL https://apt.tileception.space/tileception.gpg -o tileception.gpg
gpgv --keyring ./tileception.gpg SHA256SUMS.gpg SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
```

The checksums are signed with the same key as Tileception's packages.

## Install

Start the computer from the stick and choose **Try Ubuntu or Install
Tileception**. The live session is Ubuntu's usual desktop; Tileception comes
with the installed system.

The installer asks for your language, keyboard, network, disk, account and
time zone, as Ubuntu's does. Everything else is decided by the ISO. After
Ubuntu's part it spends a few more minutes downloading Tileception and the
apps; the progress bar can stand still meanwhile. When it asks you to remove
the installation medium, take out the stick and press **Enter**.

The installed system starts at Tileception's login screen. Installed without
a network, the system starts with Ubuntu's
desktop instead and sets up Tileception at the first start with a network.

## What you get

- Tileception, with its login screen in place of Ubuntu's.
- Ghostty as the terminal (GNOME Terminal on Ubuntu 24.04), Nautilus for
  files, Text Editor, Loupe for images, Papers for documents, File Roller for
  archives, Showtime and VLC for video, Amberol for music, LibreOffice, and
  GNOME's calculator, calendar, clocks, weather, maps, disk and system tools.
  Each is the default for its kind of file in Tileception sessions until you
  choose another in Settings → Default apps.
- Firefox and Vivaldi as debs from Mozilla's and Vivaldi's own repositories,
  and Thunderbird from Flathub.
- GNOME Software for more apps, with Flathub.
- Printing, scanning, Bluetooth, sound, power profiles and the keyring.
- Command-line tools: git, gh, htop, btop, ripgrep, bat, fd, fzf, jq, tree,
  ncdu, duf, tldr, fastfetch, tmux, neovim and zoxide.

GNOME Shell, Ubuntu's GNOME session, and snapd with Ubuntu's snaps (the
Firefox snap, the App Center and the firmware updater) are removed; `sudo apt
install snapd` brings snaps back. The apps are
recommendations of the `tileception-desktop` package, so you can remove any of
them. On a system you already have, `sudo apt install tileception-desktop`
installs the same set once [Tileception's repository](installation.md#install)
is set up.
