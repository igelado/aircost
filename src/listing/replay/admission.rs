use sqlx::{Postgres, Sqlite, Transaction};

use crate::db::AppDb;

pub(crate) const MEMBERSHIP_FROZEN_MESSAGE: &str =
    "plugin submission membership is frozen by active replay";

pub(crate) fn database_error_is_membership_freeze(error: &sqlx::Error) -> bool {
    error.to_string().contains(MEMBERSHIP_FROZEN_MESSAGE)
}

/// Exact retained-capture membership required by one replay domain commit.
///
/// Callers acquire the backend's write/membership lock before the first
/// assertion and retain it through the second assertion and commit. Provider
/// work must be completed before opening that transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TargetMembership {
    run_id: i64,
}

impl TargetMembership {
    pub(crate) fn new(run_id: i64) -> Self {
        Self { run_id }
    }

    pub(crate) async fn write_inventory_sqlite(
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar::<_, Option<i64>>(
            r#"UPDATE listing_replay_submission_inventory_lock
               SET concurrency_token = concurrency_token + 1
               WHERE singleton_id = 1 RETURNING active_run_id"#,
        )
        .fetch_one(&mut **transaction)
        .await
    }

    pub(crate) async fn write_inventory_postgres(
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar::<_, Option<i64>>(
            r#"UPDATE listing_replay_submission_inventory_lock
               SET concurrency_token = concurrency_token + 1
               WHERE singleton_id = 1 RETURNING active_run_id"#,
        )
        .fetch_one(&mut **transaction)
        .await
    }

    pub(crate) async fn matches_sqlite(
        self,
        db: &AppDb,
        transaction: &mut Transaction<'_, Sqlite>,
    ) -> Result<bool, sqlx::Error> {
        let sql = exact_membership_sql(db);
        Ok(sqlx::query_scalar::<_, i64>(&sql)
            .bind(self.run_id)
            .bind(self.run_id)
            .bind(self.run_id)
            .fetch_one(&mut **transaction)
            .await?
            == 1)
    }

    pub(crate) async fn matches_postgres(
        self,
        db: &AppDb,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<bool, sqlx::Error> {
        let sql = exact_membership_sql(db);
        Ok(sqlx::query_scalar::<_, i64>(&sql)
            .bind(self.run_id)
            .bind(self.run_id)
            .bind(self.run_id)
            .fetch_one(&mut **transaction)
            .await?
            == 1)
    }
}

fn exact_membership_sql(db: &AppDb) -> String {
    db.sql(
        r#"SELECT CASE WHEN
             (SELECT COUNT(*) FROM plugin_submissions) =
               (SELECT COUNT(*) FROM listing_replay_run_items WHERE run_id = ?)
             AND NOT EXISTS (
               SELECT 1 FROM plugin_submissions target_submission
               WHERE NOT EXISTS (
                 SELECT 1 FROM listing_replay_run_items manifest_item
                 WHERE manifest_item.run_id = ?
                   AND manifest_item.plugin_submission_id = target_submission.id
               )
             )
             AND NOT EXISTS (
               SELECT 1 FROM listing_replay_run_items manifest_item
               WHERE manifest_item.run_id = ?
                 AND NOT EXISTS (
                   SELECT 1 FROM plugin_submissions target_submission
                   WHERE target_submission.id = manifest_item.plugin_submission_id
                 )
             )
           THEN CAST(1 AS BIGINT) ELSE CAST(0 AS BIGINT) END"#,
    )
    .into_owned()
}
