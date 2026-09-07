#!/usr/bin/env bash
# Copy a verified image to an immutable tag without treating registry failures
# as proof that the target tag is absent.

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <source-reference> <target-reference>" >&2
    exit 2
fi

source_ref="$1"
target_ref="$2"

target_repository="${target_ref%:*}"
target_tag="${target_ref##*:}"
if [[ "${target_repository}" == "${target_ref}" || "${target_tag}" == */* || -z "${target_tag}" ]]; then
    echo "target reference must contain an explicit tag: ${target_ref}" >&2
    exit 2
fi

command -v crane >/dev/null 2>&1 || {
    echo "crane is required" >&2
    exit 1
}

source_digest="$(crane digest "${source_ref}")"
target_tags="$(mktemp "${RUNNER_TEMP:-/tmp}/astra-target-tags.XXXXXX")"
trap 'rm -f "${target_tags}"' EXIT HUP INT TERM

target_exists=false
if ! crane ls "${target_repository}" > "${target_tags}"; then
    echo "could not safely enumerate tags in ${target_repository}" >&2
    exit 1
fi
while IFS= read -r existing_tag; do
    if [[ "${existing_tag}" == "${target_tag}" ]]; then
        target_exists=true
        break
    fi
done < "${target_tags}"

if [[ "${target_exists}" == true ]]; then
    target_digest="$(crane digest "${target_ref}")"
    if [[ "${target_digest}" != "${source_digest}" ]]; then
        echo "${target_ref} already exists with digest ${target_digest}, expected ${source_digest}" >&2
        exit 1
    fi
    echo "verified existing immutable tag ${target_ref} -> ${source_digest}"
    exit 0
fi

crane copy --platform=all --jobs 2 "${source_ref}" "${target_ref}"
target_digest="$(crane digest "${target_ref}")"
if [[ "${target_digest}" != "${source_digest}" ]]; then
    echo "${target_ref} resolves to ${target_digest}, expected ${source_digest}" >&2
    exit 1
fi

echo "published immutable tag ${target_ref} -> ${source_digest}"
