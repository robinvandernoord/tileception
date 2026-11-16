#!/bin/sh
# Prints a checksum of everything that makes a premade ISO besides Ubuntu's
# ISO: the build script, the installer's answers, the setup files and
# Tileception's key. An ISO whose entry in manifest.json has another recipe
# needs a rebuild.
set -eu
cd "$(dirname "$0")/.."
find iso/build.sh iso/autoinstall.yaml iso/tileception packaging/tileception-archive-keyring.gpg -type f \
    | LC_ALL=C sort | xargs sha256sum | sha256sum | cut -d' ' -f1
