# worgl-server and worgl-faucet in one small image. From the repository root:
#
#   docker build -f deploy/Dockerfile -t worgl .

FROM rust:1.89-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package worgl-server --package worgl-faucet \
    && cp target/release/worgl-server target/release/worgl-faucet /usr/local/bin/

FROM debian:bookworm-slim
# Both binaries load the platform's certificate roots when they start.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /usr/local/bin/worgl-server /usr/local/bin/worgl-faucet /usr/local/bin/
COPY deploy/playground/setup.sh /usr/local/bin/worgl-playground-setup
RUN useradd --system --uid 10001 --home-dir /state worgl \
    && mkdir /state \
    && chown worgl /state
USER worgl
WORKDIR /state
ENTRYPOINT ["worgl-server"]
CMD ["--help"]
