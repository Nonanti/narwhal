# Connection vault

narwhal resolves connection passwords through a pluggable secret
backend. The default is the OS keyring; vaults are opt-in.

Resolution order, top-down:

1. `${env:...}` interpolation in the URL
2. Provider chain configured under `[vault]`
3. OS keyring (`keyring` crate)
4. `~/.pgpass` (Postgres only)
5. Interactive prompt

## OS keyring

No configuration needed. The first time you `:open` a connection,
narwhal prompts for the password and stores it under
`service = "narwhal", account = "<connection-name>"`.

Linux uses Secret Service (gnome-keyring, KWallet, …) via libdbus.
The vendored build statically links libdbus, so Alpine and minimal
containers work out of the box.

## HashiCorp Vault

```toml
[vault]
default = "main"

[[vault.providers]]
name = "main"
kind = "hashicorp"
address = "https://vault.internal:8200"
mount = "secret"
auth = "token"
token_env = "VAULT_TOKEN"

[[connection]]
name = "prod"
url  = "postgres://app@db.internal/orders"
password_ref = "vault://main/db/prod#password"
```

Supported auth methods: `token`, `approle`, `kubernetes`.

## 1Password CLI

```toml
[[vault.providers]]
name = "1p"
kind = "1password"
account = "my-team"

[[connection]]
name = "staging"
url = "postgres://app@db.staging/orders"
password_ref = "op://Vault/db-staging/password"
```

Requires the `op` CLI in `$PATH` and an active session
(`op signin`).

## AWS Secrets Manager

```toml
[[vault.providers]]
name = "aws"
kind = "aws-secrets-manager"
region = "eu-central-1"

[[connection]]
name = "ec2"
url = "postgres://app@db.aws.example/orders"
password_ref = "aws://aws/prod/orders#password"
```

Uses the standard AWS credential chain (env, profile, IMDS).

## Azure Key Vault

```toml
[[vault.providers]]
name = "az"
kind = "azure-key-vault"
vault_url = "https://my-vault.vault.azure.net"

[[connection]]
name = "azure"
url = "mssql://app@db.azure.example/orders"
password_ref = "az://az/db-orders"
```

Uses the standard Azure credential chain
(`DefaultAzureCredential`).

## Error model

Vault failures are classified so the audit log can shed light on
recurring outages:

| Class       | Meaning                                              |
|-------------|------------------------------------------------------|
| `auth`      | Provider authentication failed                       |
| `not_found` | Secret reference resolves to nothing                 |
| `network`   | Connect / read timeout                               |
| `parse`     | Reference URI is malformed                           |
| `policy`    | Provider returned 403 / equivalent                   |

The TUI surfaces the class in the status bar. The audit log records
the full taxonomy alongside the connection name.
