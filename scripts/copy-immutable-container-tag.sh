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

command -v crane >/dev/null 2>&1 || {
    echo "crane is required" >&2
    exit 1
}

source_digest="$(crane digest "${source_ref}")"
lookup_error="$(mktemp "${RUNNER_TEMP:-/tmp}/astra-target-lookup.XXXXXX")"
trap 'rm -f "${lookup_error}"' EXIT HUP INT TERM

target_exists=false
if target_digest="$(crane digest "${target_ref}" 2>"${lookup_error}")"; then
    target_exists=true
elif ! grep -Eqi \
    '(manifest unknown|name unknown|(^|[^[:alnum:]_])not found([^[:alnum:]_]|$)|HTTP[^0-9]*404([^0-9]|$)|status([^0-9]|[[:space:]]+code[[:space:]]*)[^0-9]*404([^0-9]|$))' \
    "${lookup_error}"; then
    echo "could not safely determine whether ${target_ref} exists:" >&2
    cat "${lookup_error}" >&2
    exit 1
fi

if [[ "${target_exists}" == true ]]; then
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
