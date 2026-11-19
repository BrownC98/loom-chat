#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
web=web
if command -v flock >/dev/null; then
  exec 9< "$web"
  flock -w 600 9
fi

installed=$web/node_modules/.loom-package-lock.json
stamp=$web/node_modules/.loom-web-build
if ! cmp -s "$web/package-lock.json" "$installed"; then
  npm --prefix "$web" ci
  cp "$web/package-lock.json" "$installed"
fi

fingerprint() {
  node --input-type=module - "$@" <<'JS'
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';

const [, , root, version] = process.argv;
const hash = createHash('sha256');
if (version !== undefined) {
  const variables = Object.entries(process.env)
    .filter(([name]) => name === 'NODE_ENV' || name.startsWith('VITE_')).sort();
  hash.update(JSON.stringify([version, process.version, process.platform, process.arch, variables]));
}

function addFiles(directory) {
  for (const name of readdirSync(directory).sort()) {
    if (version !== undefined && directory === root && (name === 'dist' || name === 'node_modules')) continue;
    const path = join(directory, name);
    const stat = statSync(path);
    if (stat.isDirectory()) addFiles(path);
    if (stat.isFile()) {
      hash.update(path).update('\0');
      hash.update(createHash('sha256').update(readFileSync(path)).digest());
    }
  }
}

addFiles(root);
console.log(hash.digest('hex'));
JS
}

version=$(sh scripts/version.sh)
inputs=$(fingerprint "$web" "$version")
if [[ -s "$web/dist/index.html" && -f "$stamp" ]] && output=$(fingerprint "$web/dist" 2>/dev/null); then
  if [[ "$(cat "$stamp")" == "$inputs"$'\n'"$output" ]]; then
    echo "== web build cache hit"
    exit 0
  fi
fi

rm -f "$stamp"
npm --prefix "$web" run build
test -s "$web/dist/index.html"
if [[ "$inputs" == "$(fingerprint "$web" "$(sh scripts/version.sh)")" ]]; then
  output=$(fingerprint "$web/dist")
  printf '%s\n' "$inputs" "$output" > "$stamp"
fi
