use docbox_database::{DbResult, PgConnectOptions, PgPool};

use crate::CliDatabaseConfiguration;

pub struct CliDatabaseProvider {
    pub config: CliDatabaseConfiguration,
}

impl docbox_management::database::DatabaseProvider for CliDatabaseProvider {
    fn connect(
        &self,
        database: &str,
    ) -> impl Future<Output = DbResult<docbox_database::DbPool>> + Send {
        let options = PgConnectOptions::new()
            .host(&self.config.host)
            .port(self.config.port)
            .username(&self.config.root_role_name)
            .password(&self.config.root_secret_password)
            .database(database);

        PgPool::connect_with(options)
    }
}
