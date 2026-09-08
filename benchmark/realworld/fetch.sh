#!/usr/bin/env bash
# Download and checksum-verify the non-vendored real-world revision pairs listed
# in manifest.tsv. Documents are never committed to this repository.
#
# Usage:
#   benchmark/realworld/fetch.sh [CACHE_DIR]
#
# CACHE_DIR defaults to benchmark/realworld/cache. After fetching, run:
#   cargo run -p pdfdelta-bench -- revisions --cache-dir <CACHE_DIR> --checksums-only
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="${script_dir}/manifest.tsv"
cache="${1:-${script_dir}/cache}"
mkdir -p "${cache}"

failures=0

fetch_side() {
    local pair_id="$1" side="$2" url="$3" expected_bytes="$4" expected_sha="$5"
    local target="${cache}/${pair_id}-${side}.pdf"

    if [[ "${url}" != "https://"* ]]; then
        echo "FAIL ${pair_id}-${side}: refusing non-https provenance URL ${url}" >&2
        return 1
    fi

    if [[ -f "${target}" ]]; then
        local actual_sha
        actual_sha="$(sha256sum "${target}" | cut -d' ' -f1)"
        if [[ "${actual_sha}" == "${expected_sha}" ]]; then
            echo "SKIP ${pair_id}-${side} (already present, checksum matches)"
            return 0
        fi
        echo "REFETCH ${pair_id}-${side} (existing file does not match the manifest checksum)"
    fi

    local tmp="${target}.part"
    # --max-filesize bounds the transfer at the manifest byte count before any
    # checksum work happens; the exact-size comparison below stays authoritative.
    if ! curl --fail --silent --show-error --location --retry 3 --retry-delay 2 \
            --max-filesize "${expected_bytes}" \
            --output "${tmp}" "${url}"; then
        echo "FAIL ${pair_id}-${side}: download error for ${url}" >&2
        rm -f "${tmp}"
        return 1
    fi

    local actual_bytes actual_sha
    actual_bytes="$(wc -c <"${tmp}")"
    actual_sha="$(sha256sum "${tmp}" | cut -d' ' -f1)"
    if [[ "${actual_bytes}" != "${expected_bytes}" || "${actual_sha}" != "${expected_sha}" ]]; then
        echo "FAIL ${pair_id}-${side}: provenance mismatch (bytes ${actual_bytes}/${expected_bytes}, sha256 ${actual_sha})" >&2
        rm -f "${tmp}"
        return 1
    fi
    mv "${tmp}" "${target}"
    echo "PASS ${pair_id}-${side}"
}

# The seven provenance columns follow the checksum columns and are ignored by
# this downloader; their presence keeps the fetch column offsets stable.
while IFS=$'\t' read -r pair_id old_url old_bytes old_sha new_url new_bytes new_sha; do
    if [[ ! "${pair_id}" =~ ^[A-Za-z0-9._-]+$ ]]; then
        echo "FAIL invalid pair_id in manifest: ${pair_id}" >&2
        failures=$((failures + 1))
        continue
    fi
    fetch_side "${pair_id}" old "${old_url}" "${old_bytes}" "${old_sha}" || failures=$((failures + 1))
    fetch_side "${pair_id}" new "${new_url}" "${new_bytes}" "${new_sha}" || failures=$((failures + 1))
done < <(awk -F'\t' '$0 !~ /^#/ && $1 != "pair_id" && NF > 1 {print $1 "\t" $12 "\t" $13 "\t" $14 "\t" $15 "\t" $16 "\t" $17}' "${manifest}")

if [[ "${failures}" -gt 0 ]]; then
    echo "${failures} download(s) failed provenance validation" >&2
    exit 1
fi
echo "all downloads verified against manifest checksums"
