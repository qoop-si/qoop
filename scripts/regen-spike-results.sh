#!/usr/bin/env bash
# Regenerate docs/spike-results.md from the template plus REAL program output.
#
# This exists because a review caught hand-edited transcripts in that document: the
# numbers had been reformatted and one load-bearing field deleted. Transcripts are now
# generated, never typed. CI runs this and fails on any diff.
set -euo pipefail
cd "$(dirname "$0")/.."

BINS=(spike-pty spike-term spike-proc spike-teardown spike-mem)

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  CARGO_INCREMENTAL=0 cargo build --release -p spike-runtime >/dev/null
fi

mkdir -p spikes/transcripts
for b in "${BINS[@]}"; do
  set +e
  ./target/release/"$b" > "spikes/transcripts/$b.txt" 2>&1
  rc=$?
  set -e
  echo "exit=$rc" >> "spikes/transcripts/$b.txt"
done

python3 - <<'PY'
import pathlib, re
tmpl = pathlib.Path("docs/spike-results.tmpl.md").read_text()
def sub(m):
    name = m.group(1)
    text = pathlib.Path(f"spikes/transcripts/{name}.txt").read_text().rstrip("\n")
    return text
out = re.sub(r"\{\{transcript:([a-z-]+)\}\}", sub, tmpl)
pathlib.Path("docs/spike-results.md").write_text(out)
print("wrote docs/spike-results.md")
PY
