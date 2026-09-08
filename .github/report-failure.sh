#!/usr/bin/env bash
# Writes the job summary a failure is read from: which platform, and which cases.
set -u

platform=$1
log=${2:-}

{
    echo "## ${platform} failed"

    if [ ! -s "$log" ]; then
        echo
        echo "No test log, so the failure is in a step before the suite ran: formatting, the"
        echo "build, clippy, or setting the machine up. The step's own output has it."
        exit 0
    fi

    failed=$(grep -E '^test .+ \.\.\. FAILED$' "$log" | sed -E 's/^test (.+) \.\.\. FAILED$/- \1/')
    if [ -n "$failed" ]; then
        echo
        echo "### Cases"
        echo "$failed"
    fi

    echo
    echo "### Detail"
    echo '```'
    sed -n '/^failures:$/,$p' "$log" | head -n 200
    echo '```'
} >>"${GITHUB_STEP_SUMMARY:-/dev/stdout}"
