#!/bin/bash
# Simple wrapper to run command with GNU time memory measurement

# Get GNU time from nix
TIME_BIN=$(nix-shell -p time --command 'which time' 2>/dev/null | tr -d '\n')

if [ -z "$TIME_BIN" ]; then
    echo "Error: Could not find GNU time"
    exit 1
fi

# Build command string
CMD_STR=""
for arg in "$@"; do
    # Escape arguments with spaces
    if [[ "$arg" =~ \  ]]; then
        CMD_STR="$CMD_STR \"$arg\""
    else
        CMD_STR="$CMD_STR $arg"
    fi
done

# Run command with time measurement
eval "\"$TIME_BIN\" -f \"Max RSS: %M KB\" $CMD_STR"
