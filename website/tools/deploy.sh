#!/bin/sh
# Uploads website/public to the OVH web hosting. Used by .github/workflows/website.yml.
#
#   OVH_FTP_HOST      e.g. ftp.cluster0XX.hosting.ovh.net
#   OVH_FTP_USER      the FTP login from the OVH control panel
#   OVH_FTP_PASSWORD  its password
#   OVH_FTP_PROTOCOL  ftp (default, works on every OVH plan) or sftp (plans with SSH)
#   OVH_FTP_DIR       remote web root, default www
#
# Files on the server that are not in website/public are removed.
set -eu
: "${OVH_FTP_HOST:?set OVH_FTP_HOST}" "${OVH_FTP_USER:?set OVH_FTP_USER}" "${OVH_FTP_PASSWORD:?set OVH_FTP_PASSWORD}"
proto=${OVH_FTP_PROTOCOL:-ftp}
dir=${OVH_FTP_DIR:-www}
case $proto in ftp|sftp) ;; *) echo "deploy: OVH_FTP_PROTOCOL must be ftp or sftp" >&2; exit 1 ;; esac

src="$(cd "$(dirname "$0")/../public" && pwd)"
export LFTP_PASSWORD="$OVH_FTP_PASSWORD"

lftp --env-password -u "$OVH_FTP_USER" "$proto://$OVH_FTP_HOST" <<LFTP
set cmd:fail-exit yes
set net:timeout 30
set net:max-retries 3
set net:reconnect-interval-base 5
set ftp:ssl-allow yes
set ftp:passive-mode yes
set ssl:verify-certificate yes
set sftp:auto-confirm yes
mirror --reverse --delete --verbose --no-perms --parallel=4 "$src/" "$dir/"
bye
LFTP
echo "deploy: uploaded $src to $proto://$OVH_FTP_HOST/$dir"
