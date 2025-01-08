#!/bin/bash

# A simple bash script to synchronize proto files from containerd to vendor/ directories of
# each crate.
#
# VERSION specified containerd release that script will download to extract protobuf files.
#
# For each crate, the script expects a text file named `rsync.txt` in the crate's directory.
# The file should contain a list of proto files that should be synchronized from containerd.

VERSION="v2.0.1"

set -x

# Download containerd source code.
wget https://github.com/containerd/containerd/archive/refs/tags/$VERSION.tar.gz -O containerd.tar.gz
if [ $? -ne 0 ]; then
    echo "Error: Failed to download containerd source code."
    exit 1
fi

# Ensure the file is removed on exit
trap 'rm containerd.tar.gz' EXIT

# Extract zip archive to a temporary directory.
TEMP_DIR=$(mktemp -d)
tar --extract \
    --file containerd.tar.gz \
    --strip-components=1 \
    --directory $TEMP_DIR

function sync_crate() {
    local crate_name=$1
    local temp_dir=$2

    rm -rf crates/$crate_name/vendor/github.com/containerd/containerd/

    rsync -avm \
        --include='*/' \
        --include-from=crates/$crate_name/rsync.txt \
        --exclude='*' \
        $temp_dir/ \
        crates/$crate_name/vendor/github.com/containerd/containerd/
}

sync_crate "shim_protos" $TEMP_DIR
sync_crate "snapshots" $TEMP_DIR
sync_crate "client" $TEMP_DIR
