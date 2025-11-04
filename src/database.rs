use crate::AdminDatabaseConfiguration;
use docbox_database::{DbResult, PgConnectOptions, PgPool};

pub struct CliDatabaseProvider {
    pub config: AdminDatabaseConfiguration,
    pub username: String,
    pub password: String,
}

impl docbox_management::database::DatabaseProvider for CliDatabaseProvider {
    fn connect(
        &self,
        database: &str,
    ) -> impl Future<Output = DbResult<docbox_database::DbPool>> + Send {
        let options = PgConnectOptions::new()
            .host(&self.config.host)
            .port(self.config.port)
            .username(&self.username)
            .password(&self.password)
            .database(database);

        PgPool::connect_with(options)
    }
}
