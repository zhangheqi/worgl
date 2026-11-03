# worgl-raft

Raft replication for a [Worgl](https://zhangheqi.github.io/worgl/) issuer,
built on [openraft](https://crates.io/crates/openraft). A group of replicas
keeps one mint's log, so the mint survives the loss of any minority of them,
and a registry group lists the cluster's mints.

Most people want [`worgl-server`](https://crates.io/crates/worgl-server),
which uses this crate to run as a cluster.

- [Running a cluster](https://zhangheqi.github.io/worgl/docs/running-a-cluster/)
- [API reference](https://docs.rs/worgl-raft)

> [!IMPORTANT]
> The design and code have not had an independent security review. Do not use
> Worgl to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
