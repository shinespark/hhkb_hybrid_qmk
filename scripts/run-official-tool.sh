#!/bin/zsh
set -eu

APP=/Applications/hhkb-keymap-tool.app
ROOT=${0:A:h:h}
DYLIB=$ROOT/target/debug/libhid_logger.dylib

usage() {
  print -u2 "Usage: $0 [--expect-no-device] <probe|read|keymap-write|firmware-update> [command offset]"
  exit 2
}

EXPECT_NO_DEVICE=0
if [[ ${1:-} == --expect-no-device ]]; then
  EXPECT_NO_DEVICE=1
  shift
fi

MODE=${1:-}
OFFSET=${2:-}
[[ $MODE == (probe|read|keymap-write|firmware-update) ]] || usage

if pgrep -f "$APP/Contents/MacOS/hhkb-keymap-tool" >/dev/null; then
  print -u2 "The official tool is running. Close it and retry"
  exit 1
fi

if (( EXPECT_NO_DEVICE )) && hidutil list --matching '{"VendorID":0x4fe,"ProductID":0x21}' | grep -q HHKB; then
  print -u2 "The keyboard is visible. Unplug USB, power it off, and retry"
  exit 1
fi

[[ -f $DYLIB ]] || { print -u2 "Run cargo build -p hid-logger first"; exit 1 }
codesign --verify --deep --strict "$APP"

mkdir -p "$ROOT/local/logs"
LOG=$ROOT/local/logs/$(date +%Y%m%d-%H%M%S)-$MODE.log
print "Log: $LOG"
cd "$ROOT/local/logs"

env \
  HHKB_HID_LOGGER_PATH="$LOG" \
  HHKB_HID_LOGGER_MODE="$MODE" \
  HHKB_HID_LOGGER_COMMAND_OFFSET="$OFFSET" \
  DYLD_INSERT_LIBRARIES="$DYLIB" \
  "$APP/Contents/MacOS/hhkb-keymap-tool" && STATUS=0 || STATUS=$?

print "Exit code: $STATUS"
codesign --verify --deep --strict "$APP" && print "Signature check: OK"
exit $STATUS
