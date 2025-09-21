use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::{Cell, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL};
use docbox_core::{
    aws::aws_config,
    tenant::rebuild_tenant_index::{rebuild_tenant_index, recreate_search_index_data},
};
use docbox_database::{DatabasePoolCache, DatabasePoolCacheConfig, models::tenant::TenantId};
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
use docbox_secrets::{SecretManager, SecretsManagerConfig};
use docbox_storage::{StorageLayerFactory, StorageLayerFactoryConfig};
use eyre::{Context, ContextCompat};
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::database::CliDatabaseProvider;

mod database;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to the cli configuration file if loading settings from a configuration
    /// JSON file
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    /// Name of a AWS secret manager secret containing the cli configuration, used when
    /// loading a configuration from AWS secrets manager
    #[arg(short, long)]
    pub aws_config_secret: Option<String>,

    #[arg(short, long, default_value = "human")]
    pub format: OutputFormat,
}

#[derive(ValueEnum, Clone)]
pub enum OutputFormat {
    /// Provide output in human readable format
    Human,

    /// Provide output in machine readable JSON format
    Json,
}

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

    /// Check if the root docbox database is initialized
    CheckRoot,

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

    /// Get all tenants
    GetTenants {
        // Environment to filter to
        #[arg(short, long)]
        env: Option<String>,
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
        /// Optional Name of the migration
        #[arg(short, long)]
        name: Option<String>,
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
    let args = Args::parse();
    let format = args.format.clone();

    if let Err(error) = app(args).await {
        match format {
            OutputFormat::Human => {
                return Err(error);
            }
            OutputFormat::Json => {
                tracing::error!(?error, "error occurred");

                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "error": error.to_string()
                    }))?
                );

                return Err(error);
            }
        }
    }

    Ok(())
}

async fn app(args: Args) -> eyre::Result<()> {
    // Load environment variables
    _ = dotenvy::dotenv();

    // Setup colorful error logging
    color_eyre::install()?;

    let indicatif_layer = IndicatifLayer::new();

    tracing_subscriber::registry()
        .with(
            EnvFilter::from_default_env()
                // Provide logging from docbox by default
                .add_directive("docbox=info".parse()?)
                .add_directive("docbox_core=info".parse()?)
                .add_directive("docbox_database=info".parse()?)
                .add_directive("docbox_management=info".parse()?)
                .add_directive("docbox_search=info".parse()?)
                .add_directive("docbox_secrets=info".parse()?)
                .add_directive("docbox_storage=info".parse()?)
                //
                .add_directive("aws_sdk_secretsmanager=info".parse()?)
                .add_directive("aws_runtime=info".parse()?)
                .add_directive("aws_smithy_runtime=info".parse()?)
                .add_directive("hyper_util=info".parse()?),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_line_number(false)
                .with_target(false)
                .with_file(false)
                .with_writer(indicatif_layer.get_stderr_writer()),
        )
        .with(indicatif_layer)
        .init();

    let aws_config = aws_config().await;

    // Load the config data
    let config: CliConfiguration = match (args.config, args.aws_config_secret) {
        (Some(config_path), _) => {
            let config_raw = tokio::fs::read(config_path).await?;
            let config: CliConfiguration =
                serde_json::from_slice(&config_raw).context("failed to parse config")?;
            config
        }
        (_, Some(config_secret_name)) => {
            let secrets = SecretManager::from_config(&aws_config, SecretsManagerConfig::Aws);
            secrets
                .parsed_secret(&config_secret_name)
                .await
                .context("failed to get config secret")?
                .context("config secret not found")?
        }

        _ => eyre::bail!(
            "must provided either --config or --aws-config-secret check --help for more details"
        ),
    };

    let secrets = SecretManager::from_config(&aws_config, config.secrets.clone());
    let secrets = Arc::new(secrets);

    // Setup database cache / connector
    let db_cache = Arc::new(DatabasePoolCache::from_config(
        DatabasePoolCacheConfig {
            host: config.database.host.clone(),
            port: config.database.port,
            root_secret_name: config.database.root_secret_name.clone(),
            max_connections: None,
        },
        secrets.clone(),
    ));

    let search_factory = SearchIndexFactory::from_config(
        &aws_config,
        secrets.clone(),
        db_cache,
        config.search.clone(),
    )?;
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

            match args.format {
                OutputFormat::Human => {
                    println!("successfully created root");
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "initialized": true
                        }))?
                    );
                }
            }

            Ok(())
        }

        Commands::CheckRoot => {
            let is_initialized = docbox_management::root::initialize::is_initialized(&db_provider)
                .await
                .context("failed to setup root")?;

            match args.format {
                OutputFormat::Human => {
                    if is_initialized {
                        println!("root is initialized");
                    } else {
                        println!("root is not initialized");
                    }
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "is_initialized": is_initialized
                        }))?
                    );
                }
            }

            Ok(())
        }

        Commands::CreateTenant { file } => {
            // Load the create tenant config
            let tenant_config_raw = tokio::fs::read(file).await?;
            let tenant_config: CreateTenantConfig =
                serde_json::from_slice(&tenant_config_raw).context("failed to parse config")?;

            tracing::info!(?tenant_config, "creating tenant");

            let tenant = docbox_management::tenant::create_tenant::create_tenant(
                &db_provider,
                &search_factory,
                &storage_factory,
                &secrets,
                tenant_config,
            )
            .await?;

            tracing::info!(?tenant, "tenant created successfully");

            match args.format {
                OutputFormat::Human => {
                    println!("tenant created successfully");

                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env"])
                        .add_row(vec![
                            Cell::new(tenant.id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                        ]);

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&tenant)?);
                }
            }

            Ok(())
        }

        Commands::DeleteTenant { env, tenant_id } => {
            docbox_management::tenant::delete_tenant::delete_tenant(&db_provider, &env, tenant_id)
                .await?;

            match args.format {
                OutputFormat::Human => {
                    println!("deleted tenant")
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "deleted": true
                        }))?
                    );
                }
            }

            Ok(())
        }

        Commands::GetTenants { env } => {
            let mut tenants =
                docbox_management::tenant::get_tenants::get_tenants(&db_provider).await?;

            if let Some(env) = env {
                tenants.retain(|tenant| tenant.env.eq(&env));
            }

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env"]);

                    for tenant in tenants {
                        table.add_row(vec![
                            Cell::new(tenant.id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&tenants)?);
                }
            }

            Ok(())
        }

        Commands::GetTenant { env, tenant_id } => {
            let tenant =
                docbox_management::tenant::get_tenant::get_tenant(&db_provider, &env, tenant_id)
                    .await?
                    .context("tenant not found")?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

                    table.add_row(vec![Cell::new("ID"), Cell::new(tenant.id.to_string())]);
                    table.add_row(vec![Cell::new("Name"), Cell::new(tenant.name)]);
                    table.add_row(vec![Cell::new("Env"), Cell::new(tenant.env)]);
                    table.add_row(vec![Cell::new("DB Name"), Cell::new(tenant.db_name)]);
                    table.add_row(vec![
                        Cell::new("DB Secret Name"),
                        Cell::new(tenant.db_secret_name),
                    ]);
                    table.add_row(vec![
                        Cell::new("Storage Bucket Name"),
                        Cell::new(tenant.s3_name),
                    ]);
                    table.add_row(vec![
                        Cell::new("Search Index Name"),
                        Cell::new(tenant.os_index_name),
                    ]);
                    table.add_row(vec![
                        Cell::new("Event Queue URL"),
                        Cell::new(tenant.event_queue_url.unwrap_or_default()),
                    ]);

                    println!("{table}");
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&tenant)?);
                }
            }

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

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in outcome.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }
                    for (error, tenant) in outcome.failed_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new(format!("Failed: {error}")),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&outcome)?);
                }
            }

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
                    target_migration_name: name,
                },
            )
            .await?;

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in outcome.applied_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }
                    for (error, tenant) in outcome.failed_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.tenant_id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new(format!("Failed: {error}")),
                        ]);
                    }

                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&outcome)?);
                }
            }

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

            let index_data = recreate_search_index_data(&db, &storage).await?;
            tracing::debug!("all data loaded: {}", index_data.len());

            {
                let serialized = serde_json::to_string(&index_data).unwrap();
                tokio::fs::write(file, serialized)
                    .await
                    .context("failed to write index to file")?;
            }

            rebuild_tenant_index(&db, &search, &storage)
                .await
                .context("failed to rebuild tenant index")?;

            Ok(())
        }
    }
}
