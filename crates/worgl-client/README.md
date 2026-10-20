# worgl-client

Talks to a [Worgl](https://zhangheqi.github.io/worgl/) server over HTTP.
`Client` makes one typed call per route of
[`worgl-server`](https://crates.io/crates/worgl-server), and `Purse` runs a
[`worgl-sdk`](https://crates.io/crates/worgl-sdk) holder over HTTP and saves
it after every change, keeping the holder's privacy rules.

```sh
cargo add worgl worgl-sdk worgl-client
cargo add tokio --features macros,rt-multi-thread
```

- [Rust SDK](https://zhangheqi.github.io/worgl/docs/rust-sdk/)
- [HTTP API](https://zhangheqi.github.io/worgl/docs/http-api/)
- [API reference](https://docs.rs/worgl-client)

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
