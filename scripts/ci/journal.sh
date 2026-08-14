#!/usr/bin/env bash
# reads the run's JOURNAL from the installer's output.
#
# why a separate file: these functions interpret a format, and a format goes
# wrong in silence — a pattern that does not match produces no error, it produces
# ZERO RESULTS. and zero results, in a cleanliness check, looks like "every
# package in the delta was purged" when not one was verified. living here, the
# logic can be exercised by the self-test against a faithful sample, in the fast
# CI, without waiting for a real installation.
#
# ## journal is not manifest
#
# the manifest says what is STILL on the system: when a rollback undoes a step,
# that record disappears. perfect for "what remains", useless for "what was
# done" — in probe mode the installation undoes itself and the manifest is
# rightly empty. the account of what happened lives in the log, which is not
# rewritten.
#
# ## the format's two traps, both paid for in the field
#
# 1. **the ANSI codes are there on a pipe too.** the logging layer does no
#    terminal auto-detection: it always colours. in a captured file a plain word
#    carries escapes around it, and no pattern written by looking at what one
#    READS on screen matches. the CI web view renders escapes invisible, so the
#    defect is invisible twice.
# 2. **one field is not quoted.** it is a `Display` field, not `Debug`: the value
#    is unquoted and runs to end of line, while a neighbouring string field is
#    quoted. both forms live on the same line.

# strips the ANSI codes. ALWAYS applied before any other reading.
journal_strip_ansi() {
  sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$1"
}

# the completed steps, one per line, from the progress lines.
journal_steps() {
  sed -n 's/.*progress: ✔ \([a-z0-9-]*\).*/\1/p' "$1" | sort -u
}

# the packages named by a step, one per line.
#
# `$2` is the message identifying the list, `$3` the step's name.
#
# the step is passed and not guessed: the bootstrap delta stays installed ON
# PURPOSE, and confusing it with the dependencies' delta would fail the purge
# check on minimal images — where those utilities are not preinstalled and so do
# enter the delta.
#
# an EMPTY result is legitimate (in probe mode the step may not have been
# reached) and must not be an error: the grep exits non-zero on no match, and
# under pipefail that would abort the caller. the `|| true` is part of the
# contract, not laziness.
journal_packages() {
  sed -n "s/.*package delta: $2 step=\"$3\" packages=//p" "$1" \
    | tr ' ' '\n' | grep -v '^$' | sort -u || true
}

# the interpreter chosen for the virtualenv, or empty when the system one was
# used (M11).
#
# **the order of the pieces on the line is the point.** the logger prints the
# message first and the fields after, so the field comes AFTER the text: a
# pattern looking for them in the order one thinks them — field, then message —
# matches nothing and returns silence. the same way this parsing has already
# broken twice, which is why it lives here next to a faithful sample.
journal_python_plan() {
  sed -n 's/.*the virtualenv will be built.*interpreter=\([a-z0-9.]*\).*/\1/p' "$1" | head -1
}
