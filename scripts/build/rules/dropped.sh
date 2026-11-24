#!/usr/bin/env bash
# Nothing is dropped silently (docs/build/phases/P1.md §3.13.12, CLAUDE.md "Nothing dropped
# silently"). In core/src, app/src, server/src and log/src, any line that discards a value with:
#   - `let _ = ...`
#   - a leading `_ = ...` (not a `select!` arm, which has `=>` on the line)
#   - `drop(...send / join / spawn...)`
#   - a statement ending in `.ok();`
# must carry `// dropped: <why>` on that line (the first line if multi-line).
set -euo pipefail
cd "$(dirname "$0")/../../.."

files=$(find core/src app/src server/src log/src -name '*.rs' | sort)

# shellcheck disable=SC2086 # paths have no spaces
perl -e '
  my $bad = 0;
  for my $file (@ARGV) {
    open my $in, "<", $file or die "$file: $!";
    my @lines = <$in>;
    close $in;
    for my $i (0 .. $#lines) {
      my $raw = $lines[$i];
      next if $raw =~ /^\s*\/\//;
      my $code = $raw;
      $code =~ s/(^|\s)\/\/.*$//;
      my $hit = 0;
      $hit = 1 if $code =~ /\blet\s+_\s*=/;
      $hit = 1 if !$hit && $code =~ /^\s*_\s*=/ && $code !~ /=>/;
      $hit = 1 if !$hit && $code =~ /\bdrop\([^)]*(?:send|join|spawn)/;
      $hit = 1 if !$hit && $code =~ /\.ok\(\)\s*;\s*$/;
      next unless $hit;
      next if $raw =~ /\/\/ dropped:\s*\S/;
      chomp $raw;
      $raw =~ s/^\s+//;
      print "$file:" . ($i + 1) . ": discarded without `// dropped: <why>`: $raw\n";
      $bad = 1;
    }
  }
  exit $bad;
' $files
