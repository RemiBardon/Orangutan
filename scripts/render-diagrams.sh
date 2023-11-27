#!/bin/bash

GENERATOR='https://kroki.io'
OUT_FORMAT='svg'
DEFAULT_SOURCE_DIR='diagrams'
DEFAULT_DEST_DIR='assets'

echo 'Rendering diagrams written in PlantUML, Mermaid or BPMN formats…'
echo ''

usage() {
    echo 'Usage: '"$0"' [<force> [<source_dir> [<source_dir>]]]'
    echo '  force          Empty cache directory if `-f`, do nothing otherwise. (default: NO)'
    echo '  source_dir     Path to the directory containing sources. (default: <'"${DEFAULT_SOURCE_DIR}"'>)'
    echo '  dest_dir       Path to the directory containing assets. (default: <'"${DEFAULT_DEST_DIR}"'>)'
}

error() {
    echo "Error: $1"
    echo ''
    usage
    exit 1
}

[[ ($# -le 3) ]] || error 'Bad arguments'

FORCE="$1"
SOURCE_DIR="${2:-"${DEFAULT_SOURCE_DIR}"}"
[[ -d "${SOURCE_DIR}" ]] || error "Source directory <${SOURCE_DIR}> does not exist"
DEST_DIR="${3:-"${DEFAULT_DEST_DIR}"}"
[[ -d "${DEST_DIR}" ]] || error "Destination directory <${DEST_DIR}> does not exist"
CACHE_DIR="${DEST_DIR}"/.gen-cache

# Empty cache directory if force (`-f`)
[[ "${FORCE}" == '-f' ]] && rm -rf "${CACHE_DIR}"

# See <https://stackoverflow.com/a/63970495/10967642>
strip() {
    dir_path="$(dirname "${file_path}")"

    subdir_path="${dir_path#"${SOURCE_DIR}"}"

    # Get the basename without external command
    # by stripping out longest leading match of anything followed by /
    file_name="$(basename "${file_path}")"

    # Strip last trailing extension only
    # by stripping out shortest trailing match of dot followed by anything
    file_name_no_ext="${file_name%.*}"
}

compute_vars() {
    strip
    dest="${DEST_DIR}/${subdir_path}/${file_name_no_ext}.${OUT_FORMAT}"
    file_cache="${CACHE_DIR}/${subdir_path}/${file_name}.sha"
}

compute_hash() {
    openssl dgst -sha256 "${file_path}"
}

store_hash() {
    # Create cache sub-directory if needed
    local cache_subdir="$(dirname "${file_cache}")"
    [[ -d "${cache_subdir}" ]] || mkdir -p "${cache_subdir}"

    local hash=$(compute_hash)
    echo "${hash}" > "${file_cache}"
}

is_cached() {
    # File not cached if cache file doesn't exist
    [[ -f "${file_cache}" ]] || return 1 # `1` resolves to `false`

    # File not cached if destination file doesn't exist
    [[ -f "${dest}" ]] || return 1 # `1` resolves to `false`

    cached="$(cat "${file_cache}")"
    hash="$(compute_hash)"

    # File cached only if cached hash equals file hash
    [[ "${cached}" == "${hash}" ]]
}

process() {
    type="$1"

    while read -r file_path; do
        compute_vars

        # Skip cached diagrams
        is_cached && echo "Skipping cached <${file_path}>" && continue

        # Create sub-directory if needed
        [[ -d "${DEST_DIR}/${subdir_path}" ]] || mkdir -p "${DEST_DIR}/${subdir_path}"

        echo "Generating <${file_path}>…"
        curl --data-binary "@${file_path}" "${GENERATOR}/${type}/${OUT_FORMAT}" > "${dest}" 2> /dev/null

        store_hash || error "Cannot store hash for <${file_path}>"
    done
}

find "${SOURCE_DIR}" -name '*.plantuml' | process 'plantuml'
find "${SOURCE_DIR}" -name '*.mmd' | process 'mermaid'
find "${SOURCE_DIR}" -name '*.bpmn' | process 'bpmn'

garbage_collect_cache() {
    find "${CACHE_DIR}"/* | while read -r file_path; do
        # Remove leading `.cache`
        cached="${file_path#"${CACHE_DIR}"}"
        # Remove trailing `.sha`
        cached="${cached%.*}"

        # Remove garbage cache if cached file no longer exists
        [[ -e "${SOURCE_DIR}/${cached}" ]] || rm -rf "${file_path}"
    done
}

garbage_collect_cache
