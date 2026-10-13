#!/usr/bin/env bash
# Vendors Lexxy, the rich text editor, from its Ruby gem into crates/kiln/vendor/lexxy/.
#
#   scripts/vendor-lexxy.sh 1.0.2      (or: make vendor-lexxy VERSION=1.0.2)
#
# The gem is the source because it ships Lexxy as one prebuilt ES module (the npm package
# is unbundled). The download is refused unless its SHA-256 matches the one rubygems.org
# publishes. Files are copied byte for byte; kiln adds the license header when it serves
# them. Afterwards: review THIRD-PARTY-NOTICES.md against the bundled versions printed
# below, then run `cargo test`.
set -euo pipefail

VERSION="${1:?usage: scripts/vendor-lexxy.sh <version>, e.g. 1.0.2}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="$ROOT/crates/kiln/vendor/lexxy"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "Downloading lexxy $VERSION from rubygems.org"
curl -sSfL -o "$WORK/lexxy.gem" "https://rubygems.org/downloads/lexxy-$VERSION.gem"
expected="$(curl -sSf https://rubygems.org/api/v1/versions/lexxy.json | VERSION="$VERSION" python3 -I -c '
import json, os, sys
match = [v for v in json.load(sys.stdin) if v["number"] == os.environ["VERSION"] and v.get("platform", "ruby") == "ruby"]
print(match[0]["sha"] if match else "")
')"
actual="$(shasum -a 256 "$WORK/lexxy.gem" | cut -d' ' -f1)"
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "error: the gem's SHA-256 ($actual) doesn't match rubygems.org's (${expected:-none listed}); not vendoring" >&2
  exit 1
fi
echo "Checksum matches rubygems.org: $actual"

mkdir -p "$WORK/gem/data"
tar -xf "$WORK/lexxy.gem" -C "$WORK/gem"
tar -xzf "$WORK/gem/data.tar.gz" -C "$WORK/gem/data"
SRC="$WORK/gem/data"

mkdir -p "$DEST"
cp "$SRC/app/assets/javascript/lexxy.min.js" "$DEST/lexxy.min.js"
for css in lexxy-variables lexxy-content lexxy-editor; do
  cp "$SRC/app/assets/stylesheets/$css.css" "$DEST/$css.css"
done
cp "$SRC/MIT-LICENSE" "$DEST/MIT-LICENSE"
printf '%s\n' "$VERSION" > "$DEST/VERSION"
(cd "$DEST" && shasum -a 256 lexxy.min.js lexxy-variables.css lexxy-content.css lexxy-editor.css MIT-LICENSE > SHA256SUMS)

echo
echo "Vendored lexxy $VERSION into crates/kiln/vendor/lexxy/."
echo "Libraries it bundles (check THIRD-PARTY-NOTICES.md still covers them):"
curl -sSf "https://registry.npmjs.org/@37signals/lexxy/$VERSION" | python3 -I -c '
import json, sys
deps = json.load(sys.stdin).get("dependencies", {})
for name in ("lexical", "prismjs", "marked", "dompurify"):
    print("  " + name + " " + deps.get(name, "(not listed)"))
others = sorted(n for n in deps if n not in ("lexical", "prismjs", "marked", "dompurify") and not n.startswith("@lexical/"))
if others:
    print("  NEW dependencies to add to the notices:", ", ".join(others))
' || echo "  (couldn't read the npm manifest; check the notices by hand)"
echo
echo "Next: cargo test"
