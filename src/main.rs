use clap::{Parser, Subcommand, ValueEnum};
use comfy_table::{Cell, Table, modifiers::UTF8_ROUND_CORNERS, presets::UTF8_FULL};
use docbox_management::{
    config::{ServerConfigData, load_server_config_data_secret},
    core::{
        aws::aws_config,
        tenant::{
            rebuild_tenant_index::{rebuild_tenant_index, recreate_search_index_data},
            tenant_options_ext::TenantOptionsExt,
        },
    },
    database::{DatabaseProvider, close_pool_on_drop, models::tenant::TenantId},
    server::{ManagedServer, load_managed_server},
    tenant::{
        create_tenant::CreateTenantConfig,
        delete_tenant::{DeleteTenant, DeleteTenantOptions},
        flush_tenant_cache::flush_tenant_cache,
        get_tenant::get_tenant,
        migrate_tenant_secret_to_iam::migrate_tenant_secret_to_iam,
        migrate_tenants::MigrateTenantsConfig,
        migrate_tenants_search::{MigrateTenantsSearchConfig, migrate_tenants_search},
    },
};
use eyre::{Context, ContextCompat};
use serde_json::json;
use std::path::PathBuf;
use tracing_indicatif::IndicatifLayer;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

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
        /// Whether to delete data stored within the tenant
        #[arg(short = 'c', long)]
        delete_contents: Option<bool>,
        /// Whether to delete the tenant storage bucket itself (Requires "delete-contents")
        #[arg(short = 'd', long)]
        delete_database: Option<bool>,
        /// Whether to delete the tenant search index itself (Requires "delete-contents")
        #[arg(short = 'i', long)]
        delete_search: Option<bool>,
        /// Whether to delete the tenant database itself (Requires "delete-contents")
        #[arg(short = 's', long)]
        delete_storage: Option<bool>,
        /// Whether when using AWS secrets manager to immediately delete the secret
        /// or to allow it to be recoverable for a short period of time. (Requires "delete-contents")
        ///
        /// Note: If the secret is not immediately deleted a new tenant will not be
        /// able to make use of this secret name until the 30day recovery window
        /// has ended.
        #[arg(short = 'p', long)]
        permanently_delete_secret: Option<bool>,
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

    /// Run a root migration
    MigrateRoot,

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

    /// Set the allowed CORS origins for a tenant
    /// (Overrides existing CORS configuration)
    SetAllowedStorageCorsOrigins {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// ID of the tenant to target
        #[arg(short, long)]
        tenant_id: TenantId,
        /// Allowed origins to set
        #[arg(short, long)]
        origin: Vec<String>,
    },

    /// Migrate tenants from secrets to IAM
    MigrateTenantIam {
        // Environment to target
        #[arg(short, long)]
        env: String,
        /// Specific tenant to run against
        #[arg(short, long)]
        tenant_id: Option<TenantId>,
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
    let config: ServerConfigData = match (args.config, args.aws_config_secret) {
        (Some(config_path), _) => {
            let config_raw = tokio::fs::read(config_path).await?;
            let config: ServerConfigData =
                serde_json::from_slice(&config_raw).context("failed to parse config")?;
            config
        }
        (_, Some(config_secret_name)) => {
            load_server_config_data_secret(&aws_config, &config_secret_name).await?
        }

        _ => eyre::bail!(
            "must provided either --config or --aws-config-secret check --help for more details"
        ),
    };

    let ManagedServer {
        db_cache,
        db_provider,
        secrets,
        search,
        storage,
        events,
    } = load_managed_server(&aws_config, &config).await.unwrap();

    match args.command {
        Commands::CreateRoot => {
            if config.database.root_iam {
                docbox_management::root::initialize::initialize_iam(&db_provider)
                    .await
                    .context("failed to setup root (iam)")?;
            } else if let Some(root_secret_name) = config.database.root_secret_name.as_ref() {
                docbox_management::root::initialize::initialize(
                    &db_provider,
                    &secrets,
                    root_secret_name,
                )
                .await
                .context("failed to setup root")?;
            }

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
                &search,
                &storage,
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

        Commands::DeleteTenant {
            env,
            tenant_id,
            delete_contents,
            delete_database,
            delete_search,
            delete_storage,
            permanently_delete_secret,
        } => {
            let tenant =
                docbox_management::tenant::get_tenant::get_tenant(&db_provider, &env, tenant_id)
                    .await?
                    .context("tenant not found")?;

            // Must close the connections in advance to ensure the tenant
            // database can be deleted
            db_cache.close_tenant_pool(&tenant).await;

            // Tell the API server to flush and close its database pools
            flush_tenant_cache(&config.api)
                .await
                .context("failed to flush tenant cache")?;

            docbox_management::tenant::delete_tenant::delete_tenant(
                &db_provider,
                &search,
                &storage,
                &events,
                &secrets,
                DeleteTenant {
                    env,
                    tenant_id,
                    options: DeleteTenantOptions {
                        delete_contents: delete_contents.unwrap_or_default(),
                        delete_database: delete_database.unwrap_or_default(),
                        delete_search: delete_search.unwrap_or_default(),
                        delete_storage: delete_storage.unwrap_or_default(),
                        permanently_delete_secret: permanently_delete_secret.unwrap_or_default(),
                    },
                },
            )
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
                        Cell::new(if let Some(value) = tenant.db_secret_name {
                            format!("Some({value}")
                        } else {
                            "None".to_string()
                        }),
                    ]);
                    table.add_row(vec![
                        Cell::new("DB IAM User Name"),
                        Cell::new(if let Some(value) = tenant.db_iam_user_name {
                            format!("Some({value}")
                        } else {
                            "None".to_string()
                        }),
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

        Commands::MigrateRoot => {
            docbox_management::root::migrate_root::migrate_root(&db_provider, None).await?;

            match args.format {
                OutputFormat::Human => {
                    println!("Migrations applied")
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "success": true
                        }))?
                    );
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
                &search,
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

            let search = search.create_search_index(&tenant);
            let storage = storage.create_layer(tenant.storage_layer_options());

            // Connect to the tenant database
            let db = db_provider
                .connect(&tenant.db_name)
                .await
                .context("failed to connect to tenant db")?;

            let _guard = close_pool_on_drop(&db);

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

        Commands::SetAllowedStorageCorsOrigins {
            env,
            tenant_id,
            origin,
        } => {
            let tenant =
                docbox_management::tenant::get_tenant::get_tenant(&db_provider, &env, tenant_id)
                    .await?
                    .context("tenant not found")?;

            let storage = storage.create_layer(tenant.storage_layer_options());

            storage.set_bucket_cors_origins(origin).await?;

            match args.format {
                OutputFormat::Human => {
                    println!("updated tenant allowed origins")
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "success": true
                        }))?
                    );
                }
            }

            Ok(())
        }

        Commands::MigrateTenantIam { env, tenant_id } => {
            let mut tenants =
                docbox_management::tenant::get_tenants::get_tenants(&db_provider).await?;

            tenants.retain(|tenant| {
                tenant.env.eq(&env) && tenant_id.is_none_or(|id| tenant.id.eq(&id))
            });

            let mut migrated_tenants = Vec::new();

            for mut tenant in tenants {
                if tenant.db_iam_user_name.is_some() {
                    tracing::debug!(?tenant, "skipping tenant with iam user name already set");
                    continue;
                }

                migrate_tenant_secret_to_iam(&db_provider, &secrets, &mut tenant).await?;
                migrated_tenants.push(tenant);
            }

            match args.format {
                OutputFormat::Human => {
                    let mut table = Table::new();
                    table
                        .load_preset(UTF8_FULL)
                        .apply_modifier(UTF8_ROUND_CORNERS)
                        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic)
                        .set_header(vec!["ID", "Name", "Env", "Outcome"]);

                    for tenant in migrated_tenants {
                        table.add_row(vec![
                            Cell::new(tenant.id.to_string()),
                            Cell::new(tenant.name),
                            Cell::new(tenant.env),
                            Cell::new("Success"),
                        ]);
                    }

                    println!("migrated tenants to IAM based authentication");
                    println!("{table}")
                }
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "migrated_tenants": migrated_tenants,
                            "success": true
                        }))?
                    );
                }
            }

            Ok(())
        }
    }
}
