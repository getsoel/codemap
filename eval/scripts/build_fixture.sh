#!/usr/bin/env bash
# Build a codemap index fixture from a git repo.
#
# Usage: ./eval/scripts/build_fixture.sh <repo_url> <commit> <name>
# Example: ./eval/scripts/build_fixture.sh https://github.com/expressjs/express HEAD express
#
# Outputs: eval/fixtures/<name>/index.db

set -euo pipefail

REPO_URL="${1:?Usage: build_fixture.sh <repo_url> <commit> <name>}"
COMMIT="${2:-HEAD}"
NAME="${3:?Usage: build_fixture.sh <repo_url> <commit> <name>}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
EVAL_DIR="$(dirname "$SCRIPT_DIR")"
FIXTURE_DIR="$EVAL_DIR/fixtures/$NAME"
TMPDIR="${TMPDIR:-/tmp}"
CLONE_DIR="$TMPDIR/codemap-fixture-$NAME"

echo "Building fixture: $NAME"
echo "  Repo: $REPO_URL"
echo "  Commit: $COMMIT"

# Clone
if [ -d "$CLONE_DIR" ]; then
    echo "  Using existing clone at $CLONE_DIR"
    cd "$CLONE_DIR"
    git fetch origin
else
    echo "  Cloning..."
    git clone --depth 50 "$REPO_URL" "$CLONE_DIR"
    cd "$CLONE_DIR"
fi

if [ "$COMMIT" != "HEAD" ]; then
    git checkout "$COMMIT"
fi

ACTUAL_COMMIT=$(git rev-parse --short HEAD)
echo "  Actual commit: $ACTUAL_COMMIT"

# Index
echo "  Indexing with codemap..."
codemap index

# Copy fixture
mkdir -p "$FIXTURE_DIR"
cp .codemap/index.db "$FIXTURE_DIR/index.db"

echo "  Wrote $FIXTURE_DIR/index.db"
echo "  Commit: $ACTUAL_COMMIT"
echo ""
echo "Now create eval/datasets/$NAME.json with eval cases."
echo "Use this commit in the dataset: \"commit\": \"$ACTUAL_COMMIT\""
