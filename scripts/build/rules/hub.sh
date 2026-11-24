#!/usr/bin/env bash
# The hub does no IO and spawns no tasks (docs/build/phases/P1.md §3.8, CLAUDE.md "The hub and
# the live path do no IO"): core/src/app/hub.rs is a pure state machine over its mailbox, with
# Core::start separated into core/src/app/core.rs.
#   - Forbidden in core/src/app/hub.rs: std::fs, fs::, tokio::fs, File::, reqwest,
#     spawn_blocking, tokio::spawn, .spawn(, .blocking(, .keep(, Shelf, tape::, audio::devices,
#     keys::<fn>( (type names such as keys::Service are allowed), .install)(, .clip)(.
#   - `.await` may appear at most once (receiving from the hub mailbox).
# Accepts an optional path argument (default: core/src/app/hub.rs) so older versions can be tested.
set -euo pipefail
cd "$(dirname "$0")/../../.."

target="${1:-core/src/app/hub.rs}"

perl -e '
  my ($file) = @ARGV;
  open my $in, "<", $file or die "$file: $!";
  my @lines = <$in>;
  close $in;

  my @rules = (
    [ qr/\bstd::fs\b/,                "std::fs" ],
    [ qr/(?<![A-Za-z0-9_])fs::/,      "fs::" ],
    [ qr/\btokio::fs\b/,              "tokio::fs" ],
    [ qr/\bFile::/,                   "File::" ],
    [ qr/\breqwest\b/,                "reqwest" ],
    [ qr/\bspawn_blocking\b/,         "spawn_blocking" ],
    [ qr/\btokio::spawn\b/,           "tokio::spawn" ],
    [ qr/\.spawn\s*\(/,               ".spawn(" ],
    [ qr/\.blocking\s*\(/,            ".blocking(" ],
    [ qr/\.keep\s*\(/,                ".keep(" ],
    [ qr/\bShelf\b/,                  "Shelf" ],
    [ qr/\btape::/,                   "tape::" ],
    [ qr/\baudio::devices\b/,         "audio::devices" ],
    [ qr/\bkeys::[a-z_][a-z0-9_]*\s*\(/, "keys::<fn>(" ],
    [ qr/\.install\)\s*\(/,           ".install)(" ],
    [ qr/\.clip\)\s*\(/,              ".clip)(" ],
  );

  my $bad = 0;
  my @awaits;
  for my $i (0 .. $#lines) {
    my $line = $lines[$i];
    next if $line =~ /^\s*\/\//;
    $line =~ s/(^|\s)\/\/.*$//;
    for my $r (@rules) {
      while ($line =~ /$r->[0]/g) {
        print "$file:" . ($i + 1) . ": forbidden in hub ($r->[1])\n";
        $bad = 1;
      }
    }
    while ($line =~ /\.await\b/g) {
      push @awaits, $i + 1;
    }
  }
  if (scalar(@awaits) > 1) {
    print "$file:$awaits[1]: hub may have only one .await (mailbox recv); found " . scalar(@awaits) . " at lines " . join(", ", @awaits) . "\n";
    $bad = 1;
  }
  exit $bad;
' "$target"
