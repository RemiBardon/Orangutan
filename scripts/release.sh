#!/bin/bash

SRC=src/orangutan

VERSION="$(sed -n 's/version = "\(0.1.0\)"/\1/p' "$SRC"/Cargo.toml)"
TAG="v${VERSION:?}"

echo "Creating tag '${TAG}'…"
git tag -s "${TAG}" -m "Release ${VERSION}"
echo "Pushing tag '${TAG}'…"
git push origin "${TAG}"
