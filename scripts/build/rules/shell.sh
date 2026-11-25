#!/usr/bin/env bash
# The shell never blocks (docs/build/phases/P1.md §3.9, CLAUDE.md "The shell never blocks"):
# In app/src (or an optional file/directory argument for testing older revisions):
#   1. every #[tauri::command] function is `async fn`;
#   2. no Mutex, RwLock, .lock(), or block_on;
#   3. no main-thread-blocking window queries: is_visible, is_focused, is_minimized,
#      is_maximized, inner_size, outer_size, inner_position, outer_position, scale_factor,
#      current_monitor, primary_monitor, available_monitors.
# `run_on_main_thread` (fire-and-forget) is allowed.
set -euo pipefail
cd "$(dirname "$0")/../../.."

if [ "$#" -gt 0 ]; then
  if [ -d "$1" ]; then
    files=$(find "$1" -name '*.rs' | sort)
  else
    files="$1"
  fi
else
  files=$(find app/src -name '*.rs' | sort)
fi

# shellcheck disable=SC2086 # paths have no spaces
perl -e '
  my @forbidden = (
    [ qr/\bMutex\b/,                 "Mutex" ],
    [ qr/\bRwLock\b/,                "RwLock" ],
    [ qr/\.lock\s*\(/,               ".lock()" ],
    [ qr/\bblock_on\b/,              "block_on" ],
    [ qr/\bis_visible\b/,            "is_visible" ],
    [ qr/\bis_focused\b/,            "is_focused" ],
    [ qr/\bis_minimized\b/,          "is_minimized" ],
    [ qr/\bis_maximized\b/,          "is_maximized" ],
    [ qr/\binner_size\b/,            "inner_size" ],
    [ qr/\bouter_size\b/,            "outer_size" ],
    [ qr/\binner_position\b/,        "inner_position" ],
    [ qr/\bouter_position\b/,        "outer_position" ],
    [ qr/\bscale_factor\b/,          "scale_factor" ],
    [ qr/\bcurrent_monitor\b/,       "current_monitor" ],
    [ qr/\bprimary_monitor\b/,       "primary_monitor" ],
    [ qr/\bavailable_monitors\b/,    "available_monitors" ],
  );

  my $bad = 0;
  for my $file (@ARGV) {
    open my $in, "<", $file or die "$file: $!";
    my @lines = <$in>;
    close $in;

    my $expect_cmd_fn = 0;
    for my $i (0 .. $#lines) {
      my $line = $lines[$i];
      next if $line =~ /^\s*\/\//;
      $line =~ s/(^|\s)\/\/.*$//;

      if ($line =~ /#\[\s*tauri::command\b/) {
        $expect_cmd_fn = 1;
      } elsif ($expect_cmd_fn) {
        if ($line =~ /^\s*$/ || $line =~ /^\s*#\[/) {
          # blank line or another attribute before fn
        } else {
          if ($line !~ /\basync\s+fn\b/) {
            print "$file:" . ($i + 1) . ": #[tauri::command] must be async fn\n";
            $bad = 1;
          }
          $expect_cmd_fn = 0;
        }
      }

      for my $r (@forbidden) {
        while ($line =~ /$r->[0]/g) {
          print "$file:" . ($i + 1) . ": forbidden in shell ($r->[1])\n";
          $bad = 1;
        }
      }
    }
  }
  exit $bad;
' $files
