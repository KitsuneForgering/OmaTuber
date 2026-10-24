#!/bin/sh
# Install the app, launcher and OBS plugin together for this user.
set -eu
cd "$(dirname "$0")/.."
exec make install "$@"
