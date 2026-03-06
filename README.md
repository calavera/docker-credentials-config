# docker-credentials-config

A Rust library to load Docker client configuration and credentials from `~/.docker/config.json`, including support for external credential helpers (`osxkeychain`, `secretservice`, `pass`, etc.).

## Installation

```toml
[dependencies]
docker-credentials-config = "0.1"
```

## Usage

### Load credentials for a specific registry

```rust,no_run
use docker_credentials_config::DockerConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DockerConfig::load().await?;

    if let Some(creds) = config.credentials_for_registry("https://index.docker.io/v1/") {
        println!("logged in as {:?}", creds.username);
    }

    Ok(())
}
```

### Resolve the registry from an image name

```rust,no_run
use docker_credentials_config::{DockerConfig, image_registry};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DockerConfig::load().await?;

    let registry = image_registry("gcr.io/myproject/myimage:latest");
    let creds = config.credentials_for_registry(&registry);

    Ok(())
}
```

### Collect credentials for all known registries

Useful for multi-registry operations like `Docker::build_image`:

```rust,no_run
use docker_credentials_config::DockerConfig;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = DockerConfig::load().await?;
    let all = config.all_credentials();

    for (registry, creds) in &all {
        println!("{}: {:?}", registry, creds.username);
    }

    Ok(())
}
```

## Configuration lookup order

`DockerConfig::load` searches for the configuration file in this order:

1. `$DOCKER_CONFIG/config.json` — if the `DOCKER_CONFIG` environment variable is set
2. `~/.docker/config.json` — standard Docker CLI location

If no file is found, an empty configuration is returned.

## Credential resolution

`credentials_for_registry` delegates to the [`docker_credential`](https://crates.io/crates/docker_credential) crate, which handles the full Docker credential resolution chain:

1. Per-registry helper (`credHelpers`)
2. Inline credentials in the `auths` section
3. Global credential store (`credsStore`)

## License

MIT
