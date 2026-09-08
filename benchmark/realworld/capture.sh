#!/usr/bin/env bash
# Capture and validate a real-world PDF revision pair before adding it to the
# manifest. Documents remain in the ignored local cache.
set -euo pipefail

readonly max_bytes=104857600 # 100 MiB per PDF.
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache="${script_dir}/cache"
old_tmp=""
new_tmp=""
old_target=""
new_target=""
old_created=0
new_created=0
success=0

usage() {
    cat <<'EOF'
Usage: benchmark/realworld/capture.sh <pair-id> <old-url> <new-url>

Downloads two credential-free HTTPS PDF URLs into benchmark/realworld/cache.
Each PDF is limited to 100 MiB. Existing cache files must match the newly
downloaded bytes and are never overwritten.
EOF
}

cleanup() {
    if [[ "${success}" -ne 1 ]]; then
        if [[ "${old_created}" -eq 1 && ! -L "${old_target}" && "${old_target}" -ef "${old_tmp}" ]]; then
            rm -f -- "${old_target}"
        fi
        if [[ "${new_created}" -eq 1 && ! -L "${new_target}" && "${new_target}" -ef "${new_tmp}" ]]; then
            rm -f -- "${new_target}"
        fi
    fi
    [[ -z "${old_tmp}" ]] || rm -f -- "${old_tmp}"
    [[ -z "${new_tmp}" ]] || rm -f -- "${new_tmp}"
}
trap cleanup EXIT

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
if [[ "$#" -ne 3 ]]; then
    usage >&2
    exit 2
fi

pair_id="$1"
old_url="$2"
new_url="$3"

if [[ ! "${pair_id}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ || "${#pair_id}" -gt 100 ]]; then
    echo "FAIL: pair-id must be 1-100 ASCII letters, digits, dots, underscores, or hyphens and start with a letter or digit" >&2
    exit 2
fi

validate_url() {
    local url="$1" authority
    if [[ "${url}" != https://* || "${url}" == *'?'* || "${url}" == *'#'* || "${url}" =~ [[:space:]] ]]; then
        echo "FAIL: provenance URLs must be credential-free HTTPS URLs without query strings or fragments" >&2
        return 1
    fi
    authority="${url#https://}"
    authority="${authority%%/*}"
    if [[ -z "${authority}" || "${authority}" == *'@'* ]]; then
        echo "FAIL: provenance URLs must not contain credentials" >&2
        return 1
    fi
}

validate_url "${old_url}"
validate_url "${new_url}"
mkdir -p -- "${cache}"

download() {
    local side="$1" url="$2" destination="$3"
    if ! curl --fail --silent --show-error --location --max-redirs 5 \
            --proto '=https' --proto-redir '=https' \
            --retry 3 --retry-delay 2 --max-filesize "${max_bytes}" \
            --output "${destination}" "${url}"; then
        echo "FAIL ${pair_id}-${side}: download failed" >&2
        return 1
    fi
}

validate_pdf() {
    local side="$1" file="$2" bytes
    bytes="$(wc -c <"${file}")"
    if [[ "${bytes}" -gt "${max_bytes}" ]]; then
        echo "FAIL ${pair_id}-${side}: PDF exceeds the 100 MiB limit" >&2
        return 1
    fi
    if [[ "$(LC_ALL=C head -c 5 -- "${file}")" != '%PDF-' ]]; then
        echo "FAIL ${pair_id}-${side}: response does not start with %PDF-" >&2
        return 1
    fi
}

old_tmp="$(mktemp "${cache}/.${pair_id}-old.XXXXXX.part")"
new_tmp="$(mktemp "${cache}/.${pair_id}-new.XXXXXX.part")"
download old "${old_url}" "${old_tmp}"
validate_pdf old "${old_tmp}"
download new "${new_url}" "${new_tmp}"
validate_pdf new "${new_tmp}"
if cmp -s -- "${old_tmp}" "${new_tmp}"; then
    echo "FAIL ${pair_id}: old and new PDFs are byte-identical" >&2
    exit 1
fi

old_target="${cache}/${pair_id}-old.pdf"
new_target="${cache}/${pair_id}-new.pdf"
for side in old new; do
    tmp_name="${side}_tmp"
    target_name="${side}_target"
    tmp="${!tmp_name}"
    target="${!target_name}"
    if [[ -e "${target}" || -L "${target}" ]]; then
        if [[ ! -f "${target}" || -L "${target}" ]] || ! cmp -s -- "${tmp}" "${target}"; then
            echo "FAIL ${pair_id}-${side}: existing cache file differs; refusing to overwrite it" >&2
            exit 1
        fi
    fi
done

publish() {
    local side="$1" tmp="$2" target="$3"
    if [[ ! -e "${target}" && ! -L "${target}" ]]; then
        if ! ln -- "${tmp}" "${target}"; then
            echo "FAIL ${pair_id}-${side}: could not publish cache file" >&2
            return 1
        fi
        printf -v "${side}_created" '%s' 1
    fi
    if [[ ! -f "${target}" || -L "${target}" ]] || ! cmp -s -- "${tmp}" "${target}"; then
        echo "FAIL ${pair_id}-${side}: cache file changed during capture" >&2
        return 1
    fi
}

publish old "${old_tmp}" "${old_target}"
publish new "${new_tmp}" "${new_target}"

old_bytes="$(wc -c <"${old_target}")"
old_sha="$(sha256sum "${old_target}" | cut -d' ' -f1)"
new_bytes="$(wc -c <"${new_target}")"
new_sha="$(sha256sum "${new_target}" | cut -d' ' -f1)"

echo "PASS ${pair_id}: captured and validated both PDFs"
# The final seven fields are provenance metadata. Replace the conservative
# pair-id defaults and annotation placeholder before committing a manifest row.
echo "Manifest fields (append after expected_file):"
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\tunknown\tunknown\tnone\t%s\tunused\n' \
    "${old_url}" "${old_bytes}" "${old_sha}" "${new_url}" "${new_bytes}" "${new_sha}" \
    "${pair_id}" "${pair_id}" "$(date +%F)"
success=1
