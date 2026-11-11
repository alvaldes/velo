#!/bin/sh
# velo's installer moved next to its site:
#
#   curl -fsSL https://alvaldes.github.io/velo/install.sh | sh
#
# This file stays so the address the first README printed keeps working: it
# fetches that script and runs it, with the same environment.
set -eu
curl -fsSL https://alvaldes.github.io/velo/install.sh | sh
