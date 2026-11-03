# worgl-cli

The `worgl` command holds, pays and receives
[Worgl](https://zhangheqi.github.io/worgl/) notes. The purse is one file,
encrypted under a passphrase, in the same format as the TypeScript SDK's
backups, so each can open the other's. Payment requests, payments and
handed-over notes are JSON files that the two sides exchange.

```sh
cargo install worgl-cli --locked
worgl --help
```

- [The worgl command](https://zhangheqi.github.io/worgl/docs/command-line/)
- [Quickstart](https://zhangheqi.github.io/worgl/docs/quickstart/)

> [!IMPORTANT]
> The design and code have not had an independent security review. Do not use
> Worgl to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
