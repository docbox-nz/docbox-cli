# Docbox CLI

CLI tool to setup and manage document box instances

## Create credentials file

The CLI tool require a configuration file in order to operate. See the [CLI Config](https://docbox-nz.pages.dev/docs/guides/setup/cli-config) guide to set one up

## Initialize root database

Before you can setup docbox tenants you must setup the root docbox database. See the [Create Root](https://docbox-nz.pages.dev/docs/guides/setup/create-root) guide to set this up

## Create new tenant

To create a new tenant with the CLI follow the [Create Tenant](https://docbox-nz.pages.dev/docs/guides/setup/create-tenant) guide.

# Migrations

To be documented, but runs a SQL migration over the database (With optional filtering for environment or tenant) 

```sh
 cargo run --release -p docbox-cli -- migrate --env Development --file ./packages/docbox-cli/migrations/m1_file_parent_id.sql --tenant-id 00000000-0000-0000-0000-000000000000
```