# worgl

The core of a [Worgl](https://zhangheqi.github.io/worgl/) issuer: tokens
that cost one bit to track, need no accounts, and expire if they sit still.

An issuer signs notes with Ed25519 and tracks each one with a single bit in a
layered bit array: 1 while the note is live, 0 once it is spent. A transfer
clears the input notes' bits and issues new notes to the receivers, so a note
can never be spent twice. Every so often the issuer drops its oldest layer,
which destroys the notes nobody moved for as many rotations as there are
layers.

This crate is the issuer itself: ledger, note format, transfer protocol,
policy, issuer state machine and crash-safe storage. Embed it when your
program should be the mint. To talk to a mint over HTTP, use
[`worgl-client`](https://crates.io/crates/worgl-client).

```sh
cargo add worgl
```

- [Embedding the library](https://zhangheqi.github.io/worgl/docs/embedding/)
- [Concepts](https://zhangheqi.github.io/worgl/docs/concepts/)
- [API reference](https://docs.rs/worgl)

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
