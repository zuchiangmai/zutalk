#!/usr/bin/env bash
# Numbers have names, units and reasons (docs/build/phases/P1.md §3.13.12, CLAUDE.md "Named
# numbers"). In core/src, app/src, server/src and log/src:
#   1. time has one unit, the millisecond: no Duration::from_secs, from_micros, from_nanos,
#      from_secs_f32, from_secs_f64;
#   2. Duration::from_millis( takes a name, never a bare number: a constant ending in _MS, or a
#      value worked out and named ..._ms;
#   3. no constant or static of type Duration: a constant is a number with its unit, made a
#      Duration where it is used;
#   4. the capacity of queue::bounded(, queue::threads(, queue::fanout( (the third argument) is
#      an upper-case constant whose name ends in a unit;
#   5. a constant whose name ends in a unit has a comment on its line or the line above: why it
#      is that number, or where that is written down.
# Tests are not checked. Comment lines are prose and are left out of 1-4. Each failure prints
# file:line and what to do; the script exits non-zero.
set -euo pipefail
cd "$(dirname "$0")/../../.."

# The units a name may end in, one a line.
units='
MS
US
BYTES
BLOCKS
LINES
SAMPLES
MSGS
JOBS
EVENTS
FRAMES
FILES
ROWS
TIMES
CODES
CHUNKS
PARTS
TASKS
DIRS
HZ
'

files=$(find core/src app/src server/src log/src -name '*.rs' | sort)

# shellcheck disable=SC2086 # the paths have no spaces
UNITS="$units" perl -e '
  my @units = grep { length } split /\s+/, $ENV{UNITS};
  my $unit = join "|", @units;
  my $bad = 0;
  sub say_bad { my ($file, $at, $why) = @_; print "$file:$at: $why\n"; $bad = 1 }
  for my $file (@ARGV) {
    open my $in, "<", $file or die "$file: $!";
    my @lines = <$in>;
    close $in;
    # The code alone: comment lines blanked, a trailing `// …` cut (not the `//` of a URL).
    my @code = map {
      my $line = $_;
      $line =~ /^\s*\/\// ? "\n" : do { $line =~ s/(^|\s)\/\/.*$//; $line }
    } @lines;
    my $text = join "", @code;
    my $at = sub { 1 + (substr($text, 0, $_[0]) =~ tr/\n//) };

    # 1. One unit of time.
    while ($text =~ /Duration::(from_secs_f32|from_secs_f64|from_secs|from_micros|from_nanos)\b/g) {
      say_bad($file, $at->($-[0]), "Duration::$1: time is in ms; write Duration::from_millis(<NAME>_MS)");
    }
    # 2. from_millis takes a name that ends in _MS or _ms.
    while ($text =~ /Duration::from_millis\b(\s*\(\s*)?/g) {
      my $start = $-[0];
      if (!defined $1) {
        say_bad($file, $at->($start), "Duration::from_millis without a call: write Duration::from_millis(<name>_ms)");
        next;
      }
      my $rest = substr($text, $+[0]);
      next if $rest =~ /^&?\*?(?:[A-Za-z_][A-Za-z0-9_]*(?:::|\.))*[A-Za-z_][A-Za-z0-9_]*_(?:MS|ms)\s*\)/;
      say_bad($file, $at->($start), "Duration::from_millis takes a constant ending in _MS or a value named …_ms, not a number or a sum");
    }
    # 3. No Duration constants.
    while ($text =~ /\b(?:const|static)\s+([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(?:std::time::)?Duration\b/g) {
      say_bad($file, $at->($-[0]), "$1 is a Duration constant: make it a number with its unit (<NAME>_MS: u64)");
    }
    # 4. A queue says its capacity with a named constant that has a unit.
    while ($text =~ /\bqueue::(bounded|threads|fanout)\s*(?:::\s*<[^()]*?>\s*)?\(/g) {
      my ($kind, $start, $open) = ($1, $-[0], $+[0]);
      my ($depth, $arg, @args) = (1, "");
      for my $i ($open .. length($text) - 1) {
        my $c = substr($text, $i, 1);
        $depth++ if $c =~ /[(\[{<]/;
        $depth-- if $c =~ /[)\]}>]/;
        last if $depth == 0;
        if ($c eq "," && $depth == 1) { push @args, $arg; $arg = ""; next }
        $arg .= $c;
      }
      push @args, $arg;
      my $cap = defined $args[2] ? $args[2] : "";
      $cap =~ s/^\s+|\s+$//g;
      next if $cap =~ /^(?:[A-Za-z_][A-Za-z0-9_]*::)*[A-Z][A-Z0-9_]*_(?:$unit)$/;
      say_bad($file, $at->($start), "queue::$kind: the capacity \"$cap\" is a named constant ending in a unit (numbers.sh lists them)");
    }
    # 5. A constant with a unit says why it is that number.
    for my $i (0 .. $#lines) {
      next unless $code[$i] =~ /^\s*(?:pub(?:\([a-z:]+\))?\s+)?const\s+([A-Z][A-Z0-9_]*_(?:$unit))\s*:/;
      my $name = $1;
      next if $lines[$i] =~ /(^|\s)\/\//;
      my $j = $i - 1;
      $j-- while $j >= 0 && $lines[$j] =~ /^\s*#\[/;
      next if $j >= 0 && $lines[$j] =~ /^\s*\/\//;
      say_bad($file, $i + 1, "$name has a unit and no reason: say on its line or the one above why it is that number");
    }
  }
  exit $bad;
' $files
