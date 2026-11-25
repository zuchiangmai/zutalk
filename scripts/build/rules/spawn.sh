#!/usr/bin/env bash
# Tasks and threads are started only in core/src/rt/supervise.rs (docs/build/phases/P1.md §3.13.12,
# CLAUDE.md "Every long-running task is supervised"): outside it, core/src and app/src may not name
# tokio::spawn(, task::spawn(, spawn_blocking(, spawn_local(, thread::spawn(, thread::Builder,
# async_runtime::spawn( or JoinSet.
# Existing uses not yet moved onto the tree are listed in `debt` (and server/src in `later`); every
# row's count must match exactly so debt can only shrink and new code cannot slip in.
set -euo pipefail
cd "$(dirname "$0")/../../.."

# Debt: file, count, who pays it.
debt='
'

# Server uses checked in the phase that touches the server (P1.md §3.13.12).
later='
server/src/lib.rs 1 P5
'

files=$(find core/src app/src server/src -name '*.rs' | sort)

# shellcheck disable=SC2086 # paths have no spaces
DEBT="$debt" LATER="$later" perl -e '
  my (%owed, %who, %hits);
  for my $raw ($ENV{DEBT}, $ENV{LATER}) {
    for my $line (split /\n/, $raw) {
      next unless $line =~ /^\s*(\S+)\s+(\d+)\s+(\S+)/;
      $owed{$1} = $2;
      $who{$1} = $3;
      $hits{$1} = [];
    }
  }
  my $bad = 0;
  for my $file (@ARGV) {
    next if $file eq "core/src/rt/supervise.rs";
    open my $in, "<", $file or die "$file: $!";
    my @lines = <$in>;
    close $in;
    for my $i (0 .. $#lines) {
      my $line = $lines[$i];
      next if $line =~ /^\s*\/\//;
      $line =~ s/(^|\s)\/\/.*$//;
      while ($line =~ /\b(?:tokio::spawn(?:::<[^()]*>)?\s*\(|task::spawn(?:::<[^()]*>)?\s*\(|spawn_blocking(?:::<[^()]*>)?\s*\(|spawn_local(?:::<[^()]*>)?\s*\(|thread::spawn(?:::<[^()]*>)?\s*\(|thread::Builder\b|async_runtime::spawn(?:::<[^()]*>)?\s*\(|JoinSet\b)/g) {
        push @{ $hits{$file} }, [ $i + 1, $& ];
      }
    }
  }
  for my $file (sort keys %hits) {
    my @found = @{ $hits{$file} };
    my $n = scalar @found;
    if (!exists $owed{$file}) {
      for my $h (@found) {
        print "$file:$h->[0]: task/thread spawned outside core/src/rt/supervise.rs ($h->[1])\n";
        $bad = 1;
      }
      next;
    }
    my $want = $owed{$file};
    if ($n > $want) {
      print "$file: $n spawn uses, debt is $want ($who{$file}):\n";
      for my $h (@found) {
        print "  $file:$h->[0]: $h->[1]\n";
      }
      $bad = 1;
    } elsif ($n < $want) {
      if ($n == 0) {
        print "$file: 0 spawn uses, debt says $want: remove its row in scripts/build/rules/spawn.sh\n";
      } else {
        print "$file: $n spawn uses, debt says $want: lower its row in scripts/build/rules/spawn.sh to $n\n";
      }
      $bad = 1;
    }
  }
  exit $bad;
' $files
