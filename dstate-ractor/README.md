# dstate-ractor

[Ractor](https://crates.io/crates/ractor) adapter for the [`dstate`](https://crates.io/crates/dstate) distributed state crate.

This crate implements the `ActorRuntime` trait from `dstate` using [ractor](https://crates.io/crates/ractor) actors, enabling distributed state replication in ractor-based clusters.

## Usage

```toml
[dependencies]
dstate = "0.1"
dstate-ractor = "0.1"
```

See the [dstate repository](https://github.com/Yaming-Hub/dstate) for full documentation and examples.

## License

MIT
