#!/bin/bash
set -e

rm -rf /tmp/oxfs-mount /tmp/oxfs-data /tmp/oxfs.db
mkdir -p /tmp/oxfs-mount /tmp/oxfs-data

echo "Starting oxfs..."
/app/target/release/oxfs mount /tmp/oxfs-mount \
  --backend fs --root /tmp/oxfs-data --meta-db /tmp/oxfs.db &
OXFS_PID=$!
sleep 2

# Verify mount
echo "test" > /tmp/oxfs-mount/verify.txt
cat /tmp/oxfs-mount/verify.txt
rm /tmp/oxfs-mount/verify.txt
echo "Mount verified."

echo ""
echo "Running pjdfstest..."
cd /tmp/oxfs-mount
prove -r /opt/pjdfstest/tests/ 2>&1 | tail -20

echo ""
echo "Failure breakdown:"
cd /tmp/oxfs-mount
prove -r /opt/pjdfstest/tests/ 2>&1 | grep "not ok" | grep -oP "expected \S+, got \S+" | sort | uniq -c | sort -rn | head -15

# Cleanup
kill $OXFS_PID 2>/dev/null
wait $OXFS_PID 2>/dev/null
umount /tmp/oxfs-mount 2>/dev/null || fusermount -u /tmp/oxfs-mount 2>/dev/null || true
