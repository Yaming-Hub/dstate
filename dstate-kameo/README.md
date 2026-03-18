# dstate-kameo

[Kameo](https://crates.io/crates/kameo) adapter for the [`dstate`](https://crates.io/crates/dstate) distributed state crate.

This crate implements the `ActorRuntime` trait from `dstate` using [kameo](https://crates.io/crates/kameo) actors, enabling distributed state replication in kameo-based clusters.

## Usage

```toml
[dependencies]
dstate = "0.1"
dstate-kameo = "0.1"
```

See the [dstate repository](https://github.com/Yaming-Hub/dstate) for full documentation and examples.

## License

MIT
