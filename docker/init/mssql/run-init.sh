#!/usr/bin/env bash
# The official mssql/server image has no /docker-entrypoint-initdb.d hook
# (unlike the postgres/mysql images), so docker-compose.yml runs this
# script in a short-lived sidecar container against the already-running
# server instead.
set -euo pipefail

SQLCMD=/opt/mssql-tools18/bin/sqlcmd
HOST="mssql"
PORT="1433"

echo "waiting for mssql to accept connections..."
for _ in $(seq 1 60); do
    if "$SQLCMD" -C -S "$HOST,$PORT" -U sa -P "$MSSQL_SA_PASSWORD" -Q "SELECT 1" >/dev/null 2>&1; then
        break
    fi
    sleep 2
done

for script in /init/*.sql; do
    echo "applying $script"
    "$SQLCMD" -C -S "$HOST,$PORT" -U sa -P "$MSSQL_SA_PASSWORD" -i "$script"
done

echo "mssql sample schema ready"
