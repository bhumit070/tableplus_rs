#!/bin/sh
# Adds a .deb to the apt repository in REPO_DIR and re-signs it.
#   packaging/apt/update-repo.sh REPO_DIR path/to/tabula.deb
# Needs apt-ftparchive (apt-utils) and gpg with the signing key imported;
# KEY_ID picks the key. Keeps the newest $KEEP versions (default 3) so the
# repository stays small.
set -eu
repo=$1
deb=$2
keep=${KEEP:-3}
pool=pool/main/t/tabula

mkdir -p "$repo/$pool"
version=$(dpkg-deb -f "$deb" Version)
arch=$(dpkg-deb -f "$deb" Architecture)
cp "$deb" "$repo/$pool/tabula_${version}_${arch}.deb"
ls "$repo/$pool"/tabula_*.deb | sort -V | head -n -"$keep" | xargs -r rm --

cd "$repo"
rm -rf dists
dir=dists/stable/main/binary-amd64
mkdir -p "$dir"
apt-ftparchive packages pool > "$dir/Packages"
gzip -9kn "$dir/Packages"
apt-ftparchive \
  -o APT::FTPArchive::Release::Origin=Tabula \
  -o APT::FTPArchive::Release::Label=Tabula \
  -o APT::FTPArchive::Release::Suite=stable \
  -o APT::FTPArchive::Release::Codename=stable \
  -o APT::FTPArchive::Release::Architectures=amd64 \
  -o APT::FTPArchive::Release::Components=main \
  release dists/stable > dists/stable/Release
gpg --batch --yes --local-user "$KEY_ID" --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --local-user "$KEY_ID" -abs -o dists/stable/Release.gpg dists/stable/Release
# The key users install, as a binary keyring for signed-by=.
gpg --batch --yes --export "$KEY_ID" > tabula.gpg
touch .nojekyll
