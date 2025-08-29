# Docbox CLI

CLI tool to setup and manage document box instances

# Create credentials file

The credentials file is used by the CLI to access the database with a higher level of permissions to perform things like migrations and setting up tenants. You will need this to perform a majority of the CLI actions:


Create a file name `cli-credentials.json` in the root of the repository:

```json
{
    "database": {
        "__description": "Database details and credentials",
        "host": "{DATABASE HOST}",
        "port": 5432, // DATABASE PORT
        "root_secret_name": "{ROOT DATABASE SECRET NAME}",
        "root_role_name": "{ROOT DATABASE ROLE NAME}",
        "root_secret_password": "{ROOT DATABASE SECRET PASSWORD}",
        "setup_user": {
            "__description": "User to use when migrating and setting up database, should have higher permissions",
            "username": "{DB USER}",
            "password": "{DB PASSWORD}"
        }
    },
    "secrets": {
        "__description": "Secrets manager configurations",
        "provider": "aws"
    },
    "search": {
        "__description": "Search index factory configuration",
        "provider": "typesense",
        "url": "http://localhost:8108",
        "api_key": "typesensedev"
    },
    "storage": {
        "provider": "s3",
        "endpoint": {
            "type": "aws"
        }
    }
}
```

# Initialize root database

To initialize the root database for docbox run the following command

```sh
cargo run --release -p docbox-cli -- create-root
```

# Create new tenant

Create a new file in this case `demo-tenant.json` this will contain the following:

```json
{
    "id": "00000000-0000-0000-0000-000000000000",
    "env": "{Development/Production}",
    "db_name": "docbox-{tag}-{dev/prod}",
    "db_secret_name": "postgres/docbox/{dev/prod}/{tag}",
    "db_role_name": "{TENANT DB ROLE NAME}",
    "storage_bucket_name": "docbox-{tag}-{dev/prod}",
    "search_index_name": "docbox-{tag}-{dev/prod}",
    "storage_s3_queue_arn": "arn:aws:sqs:ap-southeast-2:{YOUR_S3_UPLOADS_QUEUE_ARN}",
    "event_queue_url": "https://sqs.ap-southeast-2.amazonaws.com/{YOUR_EVENT_QUEUE_ARN}",
    "storage_cors_origins": [
        "https://{SOME_ORIGIN}"
    ]
}
```

Then run the following command from the root of this repository:

> You must ensure you have a `.env` file setup containing all the required environment 
> variables from the terraform setup
>
> Along with:
> ```
> AWS_ACCESS_KEY_ID={YOUR AWS KEY ID}
> AWS_SECRET_ACCESS_KEY={YOUR AWS SECRET ACCESS KEY}
> ```
>
> The specified key must have high enough permission to manage S3 buckets


```sh
cargo run --release -p docbox-cli -- create-tenant --file ./demo-tenant.json
```

# Migrations

To be documented, but runs a SQL migration over the database (With optional filtering for environment or tenant) 

```sh
 cargo run --release -p docbox-cli -- migrate --env Development --file ./packages/docbox-cli/migrations/m1_file_parent_id.sql --tenant-id 00000000-0000-0000-0000-000000000000
```