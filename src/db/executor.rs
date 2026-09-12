use sqlx::{PgPool, Postgres, Sqlite, SqlitePool, Transaction};

use super::{AppDb, DatabaseBackend};

// MNT-007B will replace callers' local transaction enums with this shared type.
#[allow(dead_code)]
pub(crate) enum DbTransaction {
    Sqlite(Transaction<'static, Sqlite>),
    Postgres(Transaction<'static, Postgres>),
}

#[allow(dead_code)]
impl DbTransaction {
    pub(crate) async fn commit(self) -> sqlx::Result<()> {
        match self {
            Self::Sqlite(transaction) => transaction.commit().await?,
            Self::Postgres(transaction) => transaction.commit().await?,
        }
        Ok(())
    }

    pub(crate) async fn rollback(self) -> sqlx::Result<()> {
        match self {
            Self::Sqlite(transaction) => transaction.rollback().await?,
            Self::Postgres(transaction) => transaction.rollback().await?,
        }
        Ok(())
    }
}

pub(crate) enum DbExecutor<'a> {
    SqlitePool(&'a SqlitePool),
    PostgresPool(&'a PgPool),
    SqliteTransaction(&'a mut Transaction<'static, Sqlite>),
    PostgresTransaction(&'a mut Transaction<'static, Postgres>),
}

impl<'a> From<&'a AppDb> for DbExecutor<'a> {
    fn from(db: &'a AppDb) -> Self {
        match db.backend() {
            DatabaseBackend::Sqlite(pool) => Self::SqlitePool(pool),
            DatabaseBackend::Postgres(pool) => Self::PostgresPool(pool),
        }
    }
}

impl<'a> From<&'a mut DbTransaction> for DbExecutor<'a> {
    fn from(transaction: &'a mut DbTransaction) -> Self {
        match transaction {
            DbTransaction::Sqlite(transaction) => Self::SqliteTransaction(transaction),
            DbTransaction::Postgres(transaction) => Self::PostgresTransaction(transaction),
        }
    }
}

// Shared queries are trusted static crate-authored SQL using contiguous positive
// `$1..$N` markers with exactly N binds. SQLx drivers own parsing and error
// behavior; dialect-specific or generated SQL remains in explicit backend branches.
macro_rules! db_query {
    (execute, $target:expr, $sql:expr, [$($bind:expr),* $(,)?]) => {
        $crate::db::executor::db_query!(@prepare execute, $target, $sql, [$($bind),*])
    };
    ($operation:ident($output:ty), $target:expr, $sql:expr, [$($bind:expr),* $(,)?]) => {
        $crate::db::executor::db_query!(@prepare $operation($output), $target, $sql, [$($bind),*])
    };
    (@prepare $operation:ident $(($output:ty))?, $target:expr, $sql:expr, [$($bind:expr),*]) => {{
        async {
            let executor = $crate::db::executor::DbExecutor::from($target);
            let sql: &'static str = $sql;
            let result = $crate::db::executor::db_query!(@dispatch $operation $(($output))?, executor, sql, [$($bind),*]);
            Ok::<_, sqlx::Error>(result?)
        }.await
    }};
    (@dispatch $operation:ident $(($output:ty))?, $executor:expr, $sql:expr, [$($bind:expr),*]) => {{
        match $executor {
            $crate::db::executor::DbExecutor::SqlitePool(target) =>
                $crate::db::executor::db_query!(@run $operation $(($output))?, sqlx::Sqlite, target, $sql, [$($bind),*]),
            $crate::db::executor::DbExecutor::PostgresPool(target) =>
                $crate::db::executor::db_query!(@run $operation $(($output))?, sqlx::Postgres, target, $sql, [$($bind),*]),
            $crate::db::executor::DbExecutor::SqliteTransaction(target) =>
                $crate::db::executor::db_query!(@run $operation $(($output))?, sqlx::Sqlite, &mut **target, $sql, [$($bind),*]),
            $crate::db::executor::DbExecutor::PostgresTransaction(target) =>
                $crate::db::executor::db_query!(@run $operation $(($output))?, sqlx::Postgres, &mut **target, $sql, [$($bind),*]),
        }
    }};
    (@run execute, $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query::<$database>($sql)$(.bind($bind))*
            .execute($target).await.map(|result| result.rows_affected())
    };
    (@run row_one($output:ty), $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query_as::<$database, $output>($sql)$(.bind($bind))*.fetch_one($target).await
    };
    (@run row_optional($output:ty), $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query_as::<$database, $output>($sql)$(.bind($bind))*.fetch_optional($target).await
    };
    (@run row_all($output:ty), $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query_as::<$database, $output>($sql)$(.bind($bind))*.fetch_all($target).await
    };
    (@run scalar_one($output:ty), $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query_scalar::<$database, $output>($sql)$(.bind($bind))*.fetch_one($target).await
    };
    (@run scalar_optional($output:ty), $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query_scalar::<$database, $output>($sql)$(.bind($bind))*.fetch_optional($target).await
    };
    (@run scalar_all($output:ty), $database:ty, $target:expr, $sql:expr, [$($bind:expr),*]) => {
        sqlx::query_scalar::<$database, $output>($sql)$(.bind($bind))*.fetch_all($target).await
    };
}

pub(crate) use db_query;

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;

    use super::DbTransaction;
    use crate::db::{AppDb, DatabaseBackend};

    async fn sqlite_db() -> AppDb {
        AppDb {
            backend: DatabaseBackend::Sqlite(
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect("sqlite::memory:")
                    .await
                    .unwrap(),
            ),
        }
    }

    #[tokio::test]
    async fn executor_evaluates_target_sql_and_selected_binds_once() {
        use std::cell::Cell;

        let db = sqlite_db().await;
        let target_evaluations = Cell::new(0);
        let sql_evaluations = Cell::new(0);
        let bind_evaluations = Cell::new(0);
        let value: i64 = super::db_query!(
            scalar_one(i64),
            {
                target_evaluations.set(target_evaluations.get() + 1);
                &db
            },
            {
                sql_evaluations.set(sql_evaluations.get() + 1);
                "SELECT $1"
            },
            [{
                bind_evaluations.set(bind_evaluations.get() + 1);
                42_i64
            }]
        )
        .unwrap();

        assert_eq!(value, 42);
        assert_eq!(target_evaluations.get(), 1);
        assert_eq!(sql_evaluations.get(), 1);
        assert_eq!(bind_evaluations.get(), 1);
    }

    #[tokio::test]
    async fn sqlite_driver_handles_ordinals_literals_and_comments() {
        let db = sqlite_db().await;
        let values: (i64, i64, i64) = super::db_query!(
            row_one((i64, i64, i64)),
            &db,
            "SELECT $2, $1, $2",
            [11_i64, 22_i64]
        )
        .unwrap();
        assert_eq!(values, (22, 11, 22));

        let values: (String, i64, String) = super::db_query!(
            row_one((String, i64, String)),
            &db,
            "SELECT '$2', $1, '$3' -- $4\n/* $5 */",
            [42_i64]
        )
        .unwrap();
        assert_eq!(values, ("$2".into(), 42, "$3".into()));

        let error = super::db_query!(
            scalar_one(i64),
            &db,
            "SELECT value FROM missing_executor_test_table",
            []
        )
        .unwrap_err();
        assert!(
            matches!(&error, sqlx::Error::Database(_)),
            "expected Database, got {error:?}"
        );
    }

    #[tokio::test]
    async fn sqlite_executor_supports_all_shapes_and_large_bind_lists() {
        let db = sqlite_db().await;
        assert_eq!(
            super::db_query!(
                execute,
                &db,
                "CREATE TABLE values_25 (\
                 c1 INTEGER, c2 INTEGER, c3 INTEGER, c4 INTEGER, c5 INTEGER, \
                 c6 INTEGER, c7 INTEGER, c8 INTEGER, c9 INTEGER, c10 INTEGER, \
                 c11 INTEGER, c12 INTEGER, c13 INTEGER, c14 INTEGER, c15 INTEGER, \
                 c16 INTEGER, c17 INTEGER, c18 INTEGER, c19 INTEGER, c20 INTEGER, \
                 c21 INTEGER, c22 INTEGER, c23 INTEGER, c24 INTEGER, c25 INTEGER)",
                []
            )
            .unwrap(),
            0
        );
        assert_eq!(
            super::db_query!(
                execute,
                &db,
                "INSERT INTO values_25 VALUES (\
                 $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,\
                 $16,$17,$18,$19,$20,$21,$22,$23,$24,$25)",
                [
                    1_i64, 2_i64, 3_i64, 4_i64, 5_i64, 6_i64, 7_i64, 8_i64, 9_i64, 10_i64, 11_i64,
                    12_i64, 13_i64, 14_i64, 15_i64, 16_i64, 17_i64, 18_i64, 19_i64, 20_i64, 21_i64,
                    22_i64, 23_i64, 24_i64, 25_i64,
                ]
            )
            .unwrap(),
            1
        );

        let row: (i64, i64) = super::db_query!(
            row_one((i64, i64)),
            &db,
            "SELECT c1, c25 FROM values_25",
            []
        )
        .unwrap();
        assert_eq!(row, (1, 25));
        let optional_row: Option<(i64,)> = super::db_query!(
            row_optional((i64,)),
            &db,
            "SELECT c1 FROM values_25 WHERE c1 = $1",
            [999_i64]
        )
        .unwrap();
        assert_eq!(optional_row, None);
        let rows: Vec<(i64,)> =
            super::db_query!(row_all((i64,)), &db, "SELECT c1 FROM values_25", []).unwrap();
        assert_eq!(rows, [(1,)]);
        let scalar: i64 =
            super::db_query!(scalar_one(i64), &db, "SELECT c25 FROM values_25", []).unwrap();
        assert_eq!(scalar, 25);
        let optional_scalar: Option<i64> = super::db_query!(
            scalar_optional(i64),
            &db,
            "SELECT c1 FROM values_25 WHERE c1 = $1",
            [999_i64]
        )
        .unwrap();
        assert_eq!(optional_scalar, None);
        let scalars: Vec<i64> = super::db_query!(
            scalar_all(i64),
            &db,
            "SELECT c1 FROM values_25 UNION ALL SELECT c25 FROM values_25 ORDER BY 1",
            []
        )
        .unwrap();
        assert_eq!(scalars, [1, 25]);
        assert_eq!(
            super::db_query!(
                execute,
                &db,
                "UPDATE values_25 SET c2 = $1 WHERE c1 = $2",
                [200_i64, 1_i64]
            )
            .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn sqlite_transaction_commit_rollback_and_drop_have_expected_semantics() {
        let db = sqlite_db().await;
        super::db_query!(
            execute,
            &db,
            "CREATE TABLE transaction_values (value INTEGER PRIMARY KEY)",
            []
        )
        .unwrap();

        let mut committed = db.begin().await.unwrap();
        super::db_query!(
            execute,
            &mut committed,
            "INSERT INTO transaction_values VALUES ($1)",
            [1_i64]
        )
        .unwrap();
        committed.commit().await.unwrap();

        let mut rolled_back = db.begin().await.unwrap();
        super::db_query!(
            execute,
            &mut rolled_back,
            "INSERT INTO transaction_values VALUES ($1)",
            [2_i64]
        )
        .unwrap();
        rolled_back.rollback().await.unwrap();

        {
            let mut dropped = db.begin().await.unwrap();
            super::db_query!(
                execute,
                &mut dropped,
                "INSERT INTO transaction_values VALUES ($1)",
                [3_i64]
            )
            .unwrap();
            assert!(matches!(dropped, DbTransaction::Sqlite(_)));
        }

        let values: Vec<i64> = super::db_query!(
            scalar_all(i64),
            &db,
            "SELECT value FROM transaction_values ORDER BY value",
            []
        )
        .unwrap();
        assert_eq!(values, [1]);
    }
}
