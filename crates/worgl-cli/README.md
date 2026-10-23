# worgl-cli

`worgl`: hold, pay and receive [Worgl](https://zhangheqi.github.io/worgl/)
notes from the command line. The purse lives in one file, encrypted under a
passphrase, which is also the format of the TypeScript SDK's backups: each
opens the other's. Payment requests, payments and handed-over notes are JSON
files for the two sides to exchange.

```sh
cargo install worgl-cli --locked
worgl --help
```

- [The worgl command](https://zhangheqi.github.io/worgl/docs/command-line/)
- [Quickstart](https://zhangheqi.github.io/worgl/docs/quickstart/)

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
