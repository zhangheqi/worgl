# worgl-server

An HTTP server that hosts many [Worgl](https://zhangheqi.github.io/worgl/)
mints. Each mint is one issuer, with its own key, ledger, policy and rotation
clock. Three servers can run as a cluster that keeps every mint on all three.

```sh
cargo install worgl-server --locked
worgl-server init        --state ./server
worgl-server create-mint --state ./server
worgl-server serve       --state ./server --listen 127.0.0.1:8080
```

- [Quickstart](https://zhangheqi.github.io/worgl/docs/quickstart/)
- [Running a server](https://zhangheqi.github.io/worgl/docs/running-a-server/)
- [Running a cluster](https://zhangheqi.github.io/worgl/docs/running-a-cluster/)
- [HTTP API](https://zhangheqi.github.io/worgl/docs/http-api/)

> **Status:** 0.1. The design and code have not had an independent security
> review. Do not use it to hold anything you cannot afford to lose.

## License

Licensed under either of [Apache License, Version 2.0](https://github.com/zhangheqi/worgl/blob/main/LICENSE-APACHE)
or [MIT license](https://github.com/zhangheqi/worgl/blob/main/LICENSE-MIT) at
your option.
