#!/bin/bash
# Clean gitlab-rs overlay data, then change compose image back to official GitLab CE.
# Official -> custom: change image only.
# Custom -> official: run this script first.

set -euo pipefail

WEB_CONTAINER="${WEB_CONTAINER:-gitlab-rs-cs}"
REDIS_CONTAINER="${REDIS_CONTAINER:-gitlab-rs-cs-redis}"
DB_CONTAINER="${DB_CONTAINER:-gitlab-rs-cs-db}"
DB_NAME="${DB_NAME:-gitlabhq_production}"
DB_USER="${DB_USER:-gitlab}"
SQL_FILE="$(cd "$(dirname "$0")" && pwd)/cleanup-gitlab-rs-overlay.sql"

# Delete overlay Redis keys (invite tokens and future gitlab-rs:* keys)
echo "Cleaning Redis keys gitlab-rs:* in ${REDIS_CONTAINER}"
docker exec "${REDIS_CONTAINER}" redis-cli --scan --pattern 'gitlab-rs:*' | while IFS= read -r key; do
  [ -n "${key}" ] || continue
  docker exec "${REDIS_CONTAINER}" redis-cli DEL "${key}" >/dev/null
  echo "deleted ${key}"
done

# Drop overlay SQL objects if the companion file lists any
echo "Running ${SQL_FILE} on ${DB_CONTAINER}/${DB_NAME}"
docker exec -i "${DB_CONTAINER}" psql -U "${DB_USER}" -d "${DB_NAME}" < "${SQL_FILE}"

echo "Overlay data cleaned. Point compose image to official gitlab/gitlab-ce and recreate web."
echo "Users created via invite links stay; they are normal GitLab users."
