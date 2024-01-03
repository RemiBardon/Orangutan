#!/bin/bash

SRC=src/orangutan-server

VERSION="$(rg 'version = "(\d+\.\d+.\d+)"' -N --replace '$1' "$SRC"/Cargo.toml)"
TAG="v${VERSION:?}"

echo "Creating tag '${TAG}'…"
git tag -s "${TAG}" -m "Release ${VERSION}"
echo "Pushing tag '${TAG}'…"
git push origin "${TAG}"
