# worgl-raft

Raft replication for a [Worgl](https://zhangheqi.github.io/worgl/) issuer:
one mint's log kept by a group of replicas, so the mint survives the loss of
any minority of them, and a registry group that lists a cluster's mints. It
is built on [openraft](https://crates.io/crates/openraft).

[`worgl-server`](https://crates.io/crates/worgl-server) runs on it as a
cluster, and that is what most people want.

- [Running a cluster](https://zhangheqi.github.io/worgl/docs/running-a-cluster/)
- [API reference](https://docs.rs/worgl-raft)

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
