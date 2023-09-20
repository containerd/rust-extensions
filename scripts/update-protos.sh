#!/usr/bin/env bash

DIR=$(dirname "$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )")
TEMP_DIR=`mktemp -d -p "$DIR"`

CONTAINERD_DIR="${TEMP_DIR}/containerd"
git clone https://github.com/containerd/containerd --depth 1 $CONTAINERD_DIR

# check if tmp dir was created
if [[ ! "$TEMP_DIR" || ! -d "$TEMP_DIR" ]]; then
  echo "Could not create temp dir"
  exit 1
fi

function cleanup {
  rm -rf "$TEMP_DIR"
  echo "Deleted temp working directory $TEMP_DIR"
}

# register the cleanup function to be called on the EXIT signal
trap cleanup EXIT

function update_client {
  local CRATE_DIR="${DIR}/crates/client/vendor/github.com/containerd/containerd"

  rm -rf ${CRATE_DIR}/api
  find ${CONTAINERD_DIR}/api -type f -name \*.proto ! -path '*/runtime/*' -print0 | while IFS= read -r -d $'\0' file;
    do
      cp $file "${CRATE_DIR}${file/${CONTAINERD_DIR}/''}" ;
  done
}

function update_shim_protos_containerd {
  local CRATE_DIR="${DIR}/crates/shim-protos/vendor/github.com/containerd/containerd"

  # Copy api/ proto files
  rm -rf ${CRATE_DIR}/api
  find ${CONTAINERD_DIR}/api -type f -name \*.proto -regextype posix-egrep -regex '.*\api/(events|runtime|services/ttrpc|types).*' -print0 | while IFS= read -r -d $'\0' file;
    do
      FILE="${CRATE_DIR}${file/${CONTAINERD_DIR}/''}" ;
      FILE_DIR="$(dirname "${FILE}")" ;
      mkdir -p $FILE_DIR ;
      cp $file $FILE ;
  done

  # Copy protobuf/ proto files
  rm -rf ${CRATE_DIR}/protobuf
  find ${CONTAINERD_DIR}/protobuf -type f -name \*.proto -print0 | while IFS= read -r -d $'\0' file;
    do
      FILE="${CRATE_DIR}${file/${CONTAINERD_DIR}/''}" ;
      FILE_DIR="$(dirname "${FILE}")" ;
      mkdir -p $FILE_DIR ;
      cp $file $FILE ;
  done

  # Copy runtime/ proto files
  rm -rf ${CRATE_DIR}/runtime
  find ${CONTAINERD_DIR}/runtime -type f -name \*.proto -print0 | while IFS= read -r -d $'\0' file;
    do
      FILE="${CRATE_DIR}${file/${CONTAINERD_DIR}/''}" ;
      FILE_DIR="$(dirname "${FILE}")" ;
      mkdir -p $FILE_DIR ;
      cp $file $FILE ;
  done
}

function update_shim_protos_cgroups {
  local CRATE_DIR="${DIR}/crates/shim-protos/vendor/github.com/containerd/cgroups"
  CGROUPS_DIR="${TEMP_DIR}/cgroups"
  git clone https://github.com/containerd/cgroups --depth 1 $CGROUPS_DIR

  # Copy runtime/ proto files
  rm -rf $CRATE_DIR
  find $CGROUPS_DIR -type f -name \*.proto -print0 | while IFS= read -r -d $'\0' file;
    do
      FILE="${CRATE_DIR}${file/${CGROUPS_DIR}/''}" ;
      FILE_DIR="$(dirname "${FILE}")" ;
      mkdir -p $FILE_DIR ;
      cp $file $FILE ;
    done
}

# update_client
update_shim_protos_containerd
update_shim_protos_cgroups
