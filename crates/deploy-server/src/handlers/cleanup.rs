//! Periodic database trimming, run at the start and end of a deployment.
//! Port of `src/server/databaseCleanup.ts`.

use anyhow::Result;
use rusqlite::Connection;

/// Rows are considered stale once their deployment is this old, or gone.
const STALE_AFTER_HOURS: i64 = 4;

/// How many recent deployments per project keep their manifest. Older ones are
/// only needed for history, and a manifest is by far the largest column.
const MANIFESTS_KEPT_PER_PROJECT: i64 = 5;

fn cleanup_stale_records(conn: &Connection, table_name: &str, cutoff_time: &str) -> Result<()> {
    // The table name is a compile-time constant from the call sites below, not
    // anything a client can influence.
    let sql = format!(
        "delete from {table} where deploy_name in (
            select t.deploy_name from {table} t
            left join deployment d on t.deploy_name = d.deploy_name
            where d.deploy_name is null or d.created_at < ?
        )",
        table = table_name
    );
    conn.execute(&sql, [cutoff_time])?;
    Ok(())
}

fn cleanup_old_manifests(conn: &Connection, keep_count: i64) -> Result<()> {
    let projects: Vec<String> = {
        let mut stmt = conn.prepare("select distinct project_name from deployment")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    for project_name in projects {
        conn.execute(
            "update deployment set manifest_json = null
             where project_name = ?
               and manifest_json is not null
               and deploy_name not in (
                 select deploy_name from deployment
                 where project_name = ?
                 order by created_at desc
                 limit ?
               )",
            rusqlite::params![project_name, project_name, keep_count],
        )?;
    }

    Ok(())
}

pub fn database_cleanup(conn: &Connection) -> Result<()> {
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(STALE_AFTER_HOURS))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    cleanup_stale_records(conn, "deployment_needed_file", &cutoff)?;
    cleanup_stale_records(conn, "deployment_pending_multi_part_file_chunk", &cutoff)?;
    cleanup_old_manifests(conn, MANIFESTS_KEPT_PER_PROJECT)?;

    Ok(())
}
