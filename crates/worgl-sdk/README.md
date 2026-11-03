# worgl-sdk

The holder's side of [Worgl](https://zhangheqi.github.io/worgl/): a note
store that requests, pays, receives and refreshes notes. It keeps the
privacy rules: a fresh key for every note, one note per transfer, and the
transfers of one payment spread out in time.

It does no networking and keeps no files, so it runs anywhere, including
WebAssembly. Most programs want
[`worgl-client`](https://crates.io/crates/worgl-client), which runs it over
HTTP.

With the `vault` feature, it can seal a holder under a passphrase into the
purse file that the `worgl` command uses, and open one. `Holder::merge`
restores a backup into a purse that has changed since; notes that only the
backup had count as unchecked until the mint is asked about them.

```sh
cargo add worgl worgl-sdk
```

- [Rust SDK](https://zhangheqi.github.io/worgl/docs/rust-sdk/)
- [Privacy model](https://zhangheqi.github.io/worgl/docs/privacy/)
- [API reference](https://docs.rs/worgl-sdk)

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
