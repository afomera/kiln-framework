#!/usr/bin/env bash
# Releases Kiln: bumps the version, checks everything, commits and tags, then (after asking)
# publishes kiln-framework, kiln-framework-macros and kiln-framework-cli to crates.io.
#
#   scripts/release.sh 0.0.2      (or: make release VERSION=0.0.2)
#
# 1. Refuses unless VERSION is x.y.z and newer than the current one, the working tree is
#    clean, and the tag doesn't exist yet.
# 2. Sets the workspace version and the internal dependency versions in Cargo.toml (and
#    Cargo.lock). `kiln new` from the published CLI then writes the same version into apps.
# 3. Checks: formatting, clippy with warnings as errors, the test suite, and a dry run of
#    publishing all three crates. Any failure puts Cargo.toml and Cargo.lock back.
# 4. Commits "Release vX.Y.Z" and tags vX.Y.Z.
# 5. Asks before `cargo publish --workspace`: a published version can't be changed or
#    deleted (only yanked). Pushing is left to you (`git push && git push --tags`).
set -euo pipefail

VERSION="${1:?usage: scripts/release.sh <version>, e.g. 0.0.2}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: versions look like 0.0.2 (got $VERSION)" >&2
  exit 1
fi

CURRENT="$(python3 -I -c '
import re, sys
text = open("Cargo.toml").read()
section = text.split("[workspace.package]", 1)[1].split("\n[", 1)[0]
print(re.search(r"^version = \"([^\"]+)\"", section, re.M).group(1))
')"

if ! python3 -I -c 'import sys; a, b = (tuple(map(int, v.split("."))) for v in sys.argv[1:]); sys.exit(0 if a > b else 1)' "$VERSION" "$CURRENT"; then
  echo "error: $VERSION isn't newer than the current version $CURRENT" >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "error: commit or stash your changes first; a release is cut from a clean tree" >&2
  exit 1
fi
if git rev-parse -q --verify "refs/tags/v$VERSION" >/dev/null; then
  echo "error: tag v$VERSION already exists" >&2
  exit 1
fi

# Until the release commit exists, any failure leaves the tree as it was.
committed=0
restore() {
  if [ "$committed" = 0 ]; then
    git checkout -- Cargo.toml Cargo.lock 2>/dev/null || true
    echo "release stopped; Cargo.toml and Cargo.lock are back at $CURRENT" >&2
  fi
}
trap restore EXIT

echo "==> $CURRENT -> $VERSION"
CURRENT="$CURRENT" VERSION="$VERSION" python3 -I -c '
import os, re
current, version = os.environ["CURRENT"], os.environ["VERSION"]
text = open("Cargo.toml").read()
head, rest = text.split("[workspace.package]", 1)
section, tail = rest.split("\n[", 1)
section = re.sub(r"^version = \"[^\"]+\"", f"version = \"{version}\"", section, count=1, flags=re.M)
tail = re.sub(
    r"^(kiln(?:-macros)? = \{ path = \"crates/[^\"]+\", package = \"[^\"]+\", version = \")" + re.escape(current) + "\"",
    lambda m: m.group(1) + version + "\"",
    tail,
    flags=re.M,
)
text = head + "[workspace.package]" + section + "\n[" + tail
if text.count(f"version = \"{version}\"") < 3:
    raise SystemExit("error: could not find the workspace version and both internal dependency versions in Cargo.toml")
open("Cargo.toml", "w").write(text)
'
cargo update --workspace --quiet

echo "==> checking"
cargo fmt --all --check
cargo clippy --quiet --workspace --all-targets -- -D warnings
cargo test --quiet --workspace
cargo publish --workspace --dry-run --allow-dirty

git commit --quiet -am "Release v$VERSION"
git tag -a "v$VERSION" -m "Kiln v$VERSION"
committed=1
echo "==> committed and tagged v$VERSION"

if [ -t 0 ]; then
  read -r -p "Publish v$VERSION to crates.io now? This can't be undone. [y/N] " answer
else
  answer=""
fi
if [ "$answer" = "y" ] || [ "$answer" = "Y" ]; then
  cargo publish --workspace
  echo "==> published v$VERSION: https://crates.io/crates/kiln-framework"
else
  echo "Not published. When you're ready: cargo publish --workspace"
fi
if git remote | grep -q .; then
  echo "Then push: git push && git push --tags"
fi
