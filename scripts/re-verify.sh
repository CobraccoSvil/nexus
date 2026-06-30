#!/usr/bin/env bash
set +e
cd /home/administrator/ideai
echo "=== rilancio CI=1 cargo test workspace ==="
CI=1 cargo test --workspace --no-fail-fast 2>&1 | tee /tmp/backlog-closure/cargo-test.log | grep -E "FAILED|test result|target failed|fail|error\[" | tail -50
echo "--- exit=$? ---"
