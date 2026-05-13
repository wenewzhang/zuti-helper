#!/bin/bash
echo "=== Code Statistics ==="
echo "--- Summary ---"
RS_LINES=$(find ./src -type f -name "*.rs" | xargs wc -l 2>/dev/null | tail -n 1 | awk '{print $1}')

echo "Rust code  (.rs) lines: ${RS_LINES:-0}"
