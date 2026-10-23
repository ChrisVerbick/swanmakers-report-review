#!/bin/sh
# Nightly backup, run from cron on the server (see README, "Deploy", step 9):
# a consistent copy of the SQLite file (VACUUM INTO; the last 14 kept) and the bundles,
# both synced to the bucket Litestream uses. Needs deploy/.env for the bucket settings.
set -eu
cd "$(dirname "$0")"
. ./.env
STAMP=$(date +%F)
# The data volume: `deploy_data` from the plain Compose stack, or Coolify's prefixed name.
DATA_VOLUME=${DATA_VOLUME:-deploy_data}
S3="--s3-provider Other --s3-endpoint $LITESTREAM_ENDPOINT --s3-access-key-id $LITESTREAM_ACCESS_KEY_ID --s3-secret-access-key $LITESTREAM_SECRET_ACCESS_KEY"
docker run --rm -v "$DATA_VOLUME":/data -v deploy_backup:/backup keinos/sqlite3:latest \
  sqlite3 /data/db.sqlite "VACUUM INTO '/backup/db-$STAMP.sqlite'"
docker run --rm -v deploy_backup:/backup alpine:3 find /backup -name 'db-*.sqlite' -mtime +14 -delete
docker run --rm -v "$DATA_VOLUME":/data rclone/rclone:1 sync /data/versions ":s3:$LITESTREAM_BUCKET/review/versions" $S3
docker run --rm -v deploy_backup:/backup rclone/rclone:1 sync /backup ":s3:$LITESTREAM_BUCKET/review/backups" $S3
echo "$STAMP backup done"
