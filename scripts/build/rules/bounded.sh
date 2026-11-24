#!/usr/bin/env bash
# Channels are built only in core/src/rt/queue.rs (docs/build/phases/P1.md §3.13.12, CLAUDE.md
# "No unbounded queue"): outside it, core/src and app/src may not name mpsc::channel(,
# sync_channel(, broadcast::channel(, unbounded_channel, UnboundedSender or UnboundedReceiver;
# RingBuffer::new may appear only in core/src/capture/audio.rs; clippy.toml bans both unbounded
# channel constructors workspace-wide.
# Existing uses not yet rewritten are listed in `debt` (and server/src in `later`); every row's
# count must match exactly so debt can only shrink and new code cannot slip in.
set -euo pipefail
cd "$(dirname "$0")/../../.."

if [ ! -f clippy.toml ]; then
  echo "clippy.toml is missing: unbounded channels must stay banned"
  exit 1
fi
for banned in "tokio::sync::mpsc::unbounded_channel" "std::sync::mpsc::channel"; do
  if ! grep -vE '^[[:space:]]*#' clippy.toml | grep -Fq "\"$banned\""; then
    echo "clippy.toml must ban $banned in disallowed-methods"
    exit 1
  fi
done

# Debt: file, count, who pays it.
debt='
core/src/app/hub.rs 2 P1-16
core/src/ai/write.rs 1 P1-21
core/src/out/city.rs 1 P1-21
'

# Server uses checked in the phase that touches the server (P1.md §3.13.12).
later='
server/src/room.rs 1 P5
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
    open my $in, "<", $file or die "$file: $!";
    my @lines = <$in>;
    close $in;
    for my $i (0 .. $#lines) {
      my $line = $lines[$i];
      next if $line =~ /^\s*\/\//;
      $line =~ s/(^|\s)\/\/.*$//;
      if ($file ne "core/src/rt/queue.rs") {
        while ($line =~ /\b(?:mpsc::channel(?:::<[^()]*>)?\s*\(|sync_channel(?:::<[^()]*>)?\s*\(|broadcast::channel(?:::<[^()]*>)?\s*\(|unbounded_channel\b|UnboundedSender\b|UnboundedReceiver\b)/g) {
          push @{ $hits{$file} }, [ $i + 1, $& ];
        }
      }
      if ($file ne "core/src/capture/audio.rs") {
        while ($line =~ /\bRingBuffer::new\b/g) {
          push @{ $hits{$file} }, [ $i + 1, $& ];
        }
      }
    }
  }
  for my $file (sort keys %hits) {
    my @found = @{ $hits{$file} };
    my $n = scalar @found;
    if (!exists $owed{$file}) {
      for my $h (@found) {
        print "$file:$h->[0]: channel built outside core/src/rt/queue.rs ($h->[1])\n";
        $bad = 1;
      }
      next;
    }
    my $want = $owed{$file};
    if ($n > $want) {
      print "$file: $n channel uses, debt is $want ($who{$file}):\n";
      for my $h (@found) {
        print "  $file:$h->[0]: $h->[1]\n";
      }
      $bad = 1;
    } elsif ($n < $want) {
      if ($n == 0) {
        print "$file: 0 channel uses, debt says $want: remove its row in scripts/build/rules/bounded.sh\n";
      } else {
        print "$file: $n channel uses, debt says $want: lower its row in scripts/build/rules/bounded.sh to $n\n";
      }
      $bad = 1;
    }
  }
  exit $bad;
' $files
