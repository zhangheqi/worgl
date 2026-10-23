# worgl-sdk

The holder's side of [Worgl](https://zhangheqi.github.io/worgl/): a note
store that requests, pays, receives and refreshes notes while keeping the
privacy rules. A fresh key for every note, one note per transfer, and the
transfers of one payment spread out in time.

It does no networking and keeps no files, so it runs anywhere, WebAssembly
included. [`worgl-client`](https://crates.io/crates/worgl-client) runs it
over HTTP, which is what most programs want. With the `vault` feature, it
seals a holder under a passphrase into the purse file the `worgl` command
line keeps, and opens one; `Holder::merge` takes a backup back into a purse
that moved on since.

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
