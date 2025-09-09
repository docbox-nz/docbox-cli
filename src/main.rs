use clap::{Parser, Subcommand};
use docbox_core::{
    aws::aws_config,
    tenant::rebuild_tenant_index::{rebuild_tenant_index, recreate_search_index_data},
};
use docbox_database::models::tenant::TenantId;
use docbox_management::{
    database::DatabaseProvider,
    tenant::{
        create_tenant::CreateTenantConfig,
        get_tenant::get_tenant,
        migrate_tenants::MigrateTenantsConfig,
        migrate_tenants_search::{MigrateTenantsSearchConfig, migrate_tenants_search},
    },
};
use docbox_search::{SearchIndexFactory, SearchIndexFactoryConfig};
use docbox_secrets::{AppSecretManager, SecretsManagerConfig};
use docbox_storage::{StorageLayerFactory, StorageLayerFactoryConfig};
use eyre::{Context, ContextCompat};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

use crate::database::CliDatabaseProvider;

mod database;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to the cli configuration file
    #[arg(short, long)]
    pub config: PathBuf,
}

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct AnyhowError(anyhow::Error);

#[derive(Clone, Deserialize)]
pub struct CliConfiguration {
    pub database: CliDatabaseConfiguration,
    pub secrets: SecretsManagerConfig,
    pub search: SearchIndexFactoryConfig,
    pub storage: StorageLayerFactoryConfig,
}

#[derive(Clone, Deserialize)]
pub struct CliDatabaseConfiguration {
    pub host: String,
    pub port: u16,
    pub setup_user: Option<CliDatabaseSetupUserConfig>,
    pub setup_user_secret_name: Option<String>,
    pub root_secret_name: String,
}

#[derive(Clone, Deserialize)]
pub struct CliDatabaseSetupUserConfig {
    #[serde(alias = "user")]
    pub username: String,
    pub password: String,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize the root docbox database
    CreateRoot,

    /// Create a new tenant
    CreateTenant {
        /// File containing the tenant configuration details
        #[arg(short, long)]
        file: PathBuf,
    },

    /// Rebuild the tenant search index from its files
    RebuildTenantIndex {
        /// Environment of the tenant
        #[arg(short, long)]
        env: String,

        /// ID of the tenant to rebuild
        #[arg(short, long)]
        tenant_id: TenantId,

        /// File to save the rebuilt index to in case of failure
        #[arg(short, long)]
        file: PathBuf,
    },

    /// Delete a tenant
    DeleteTenant {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to delete
        #[arg(short, long)]
        tenant_id: TenantId,
    },

    /// Get a tenant
    GetTenant {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to delete
        #[arg(short, long)]
        tenant_id: TenantId,
    },

    /// Run a migration
    Migrate {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
        #[arg(short, long)]
        skip_failed: bool,
    },

    /// Run a search migration
    MigrateSearch {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Name of the migration
        #[arg(short, long)]
        name: String,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
        /// Skip failed migrations
        #[arg(short, long)]
        skip_failed: bool,
    },
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    // Load environment variables
    _ = dotenvy::dotenv();

    // Setup colorful error logging
    color_eyre::install()?;

    // Start configuring a `fmt` subscriber
    let subscriber = tracing_subscriber::fmt()
        // Use the logging options from env variables
        .with_env_filter("aws_sdk_secretsmanager=info,aws_runtime=info,aws_smithy_runtime=info,hyper_util=info,debug")
        // Display source code file paths
        .with_file(true)
        // Display source code line numbers
        .with_line_number(true)
        // Don't display the event's target (module path)
        .with_target(false)
        // Build the subscriber
        .finish();

    // use that subscriber to process traces emitted after this point
    tracing::subscriber::set_global_default(subscriber)?;

    let args = Args::parse();

    // Load the create tenant config
    let config_raw = tokio::fs::read(args.config).await?;
    let config: CliConfiguration =
        serde_json::from_slice(&config_raw).context("failed to parse config")?;

    let aws_config = aws_config().await;
    let secrets = AppSecretManager::from_config(&aws_config, config.secrets.clone());
    let secrets = Arc::new(secrets);
    let search_factory =
        SearchIndexFactory::from_config(&aws_config, secrets.clone(), config.search.clone())
            .map_err(AnyhowError)?;
    let storage_factory = StorageLayerFactory::from_config(&aws_config, config.storage.clone());

    let db_provider = match (
        config.database.setup_user.as_ref(),
        config.database.setup_user_secret_name.as_deref(),
    ) {
        (Some(setup_user), _) => CliDatabaseProvider {
            config: config.database.clone(),
            username: setup_user.username.clone(),
            password: setup_user.password.clone(),
        },
        (_, Some(setup_user_secret_name)) => {
            let secret: CliDatabaseSetupUserConfig = secrets
                .parsed_secret(setup_user_secret_name)
                .await
                .map_err(AnyhowError)
                .context("failed to get setup user database secret")?
                .context("setup user database secret not found")?;

            tracing::debug!("loaded database secrets from secret manager");

            CliDatabaseProvider {
                config: config.database.clone(),
                username: secret.username.clone(),
                password: secret.password.clone(),
            }
        }
        (None, None) => {
            return Err(eyre::eyre!(
                "must provided either setup_user or setup_user_secret_name in database config"
            ));
        }
    };

    match args.command {
        Commands::CreateRoot => {
            docbox_management::root::initialize::initialize(
                &db_provider,
                &secrets,
                &config.database.root_secret_name,
            )
            .await
            .context("failed to setup root")?;
            Ok(())
        }

        Commands::CreateTenant { file } => {
            // Load the create tenant config
            let tenant_config_raw = tokio::fs::read(file).await?;
            let tenant_config: CreateTenantConfig =
                serde_json::from_slice(&tenant_config_raw).context("failed to parse config")?;

            tracing::debug!(?tenant_config, "creating tenant");

            let tenant = docbox_management::tenant::create_tenant::create_tenant(
                &db_provider,
                &search_factory,
                &storage_factory,
                &secrets,
                tenant_config,
            )
            .await?;

            tracing::info!(?tenant, "tenant created successfully");
            Ok(())
        }

        Commands::DeleteTenant { env, tenant_id } => {
            docbox_management::tenant::delete_tenant::delete_tenant(&db_provider, &env, tenant_id)
                .await?;
            Ok(())
        }

        Commands::GetTenant { env, tenant_id } => {
            let tenant =
                docbox_management::tenant::get_tenant::get_tenant(&db_provider, &env, tenant_id)
                    .await?
                    .context("tenant not found")?;
            tracing::debug!(?tenant, "found tenant");

            Ok(())
        }

        Commands::Migrate {
            env,
            tenant_id,
            skip_failed,
        } => {
            let outcome = docbox_management::tenant::migrate_tenants::migrate_tenants(
                &db_provider,
                MigrateTenantsConfig {
                    env: Some(env),
                    tenant_id,
                    skip_failed,
                    target_migration_name: None,
                },
            )
            .await?;

            tracing::debug!(?outcome, "completed migrations");
            Ok(())
        }

        Commands::MigrateSearch {
            env,
            name,
            tenant_id,
            skip_failed,
        } => {
            let outcome = migrate_tenants_search(
                &db_provider,
                &search_factory,
                MigrateTenantsSearchConfig {
                    env: Some(env),
                    tenant_id,
                    skip_failed,
                    target_migration_name: Some(name),
                },
            )
            .await?;

            tracing::debug!(?outcome, "migration complete");
            Ok(())
        }

        Commands::RebuildTenantIndex {
            env,
            tenant_id,
            file,
        } => {
            let tenant = get_tenant(&db_provider, &env, tenant_id)
                .await?
                .context("tenant not found")?;

            let search = search_factory.create_search_index(&tenant);
            let storage = storage_factory.create_storage_layer(&tenant);

            // Connect to the tenant database
            let db = db_provider
                .connect(&tenant.db_name)
                .await
                .context("failed to connect to tenant db")?;

            let index_data = recreate_search_index_data(&db, &storage)
                .await
                .map_err(AnyhowError)?;
            tracing::debug!("all data loaded: {}", index_data.len());

            {
                let serialized = serde_json::to_string(&index_data).unwrap();
                tokio::fs::write(file, serialized)
                    .await
                    .context("failed to write index to file")?;
            }

            rebuild_tenant_index(&db, &search, &storage)
                .await
                .map_err(AnyhowError)
                .context("failed to rebuild tenant index")?;
            Ok(())
        }
    }
}
