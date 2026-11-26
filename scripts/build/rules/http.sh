#!/usr/bin/env bash
# Every outbound HTTP client in core/src and app/src must come from `core/src/rt/http.rs` so
# connect_timeout and read_timeout are bounded everywhere (T3/T6, K1).
set -euo pipefail
cd "$(dirname "$0")/../../.."

if [ "$#" -gt 0 ]; then
  if [ -d "$1" ]; then
    files=$(find "$1" -name '*.rs' | sort)
  else
    files="$1"
  fi
else
  files=$(find core/src app/src -name '*.rs' ! -path 'core/src/rt/http.rs' | sort)
fi

# shellcheck disable=SC2086 # paths have no spaces
perl -e '
  my $bad = 0;
  for my $file (@ARGV) {
    next if $file =~ m{(?:^|/)core/src/rt/http\.rs$};
    open my $in, "<", $file or die "$file: $!";
    my $n = 0;
    while (my $line = <$in>) {
      $n++;
      next if $line =~ /^\s*\/\//;
      $line =~ s/(^|\s)\/\/.*$//;
      if ($line =~ /\bClient::(?:new|builder)\s*\(/) {
        print "$file:$n: unbounded HTTP client construction forbidden outside core/src/rt/http.rs\n";
        $bad = 1;
      }
    }
    close $in;
  }
  exit $bad;
' $files
