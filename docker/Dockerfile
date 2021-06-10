FROM rust:1-alpine as appgated-builder
LABEL builder=true
LABEL appgate=true
LABEL project=appgated
WORKDIR /usr/src/appgate-appgated
RUN apk add --no-cache musl-dev && apk add --no-cache openssl-dev
RUN cargo init
COPY Cargo.lock .
COPY Cargo.toml .
RUN cargo build --release
RUN rm src/*rs
RUN rm target/release/deps/appgate_appgated*
COPY src src
RUN cargo build --release

FROM alpine
LABEL appgate=true
LABEL project=appgated
COPY --from=appgated-builder /usr/src/appgate-appgated/target/release/appgate-appgated /bin/appgate-appgated
WORKDIR /appgate-inject
COPY sidecars.json .
COPY certs/sdp-injector-key.pem certs/private-key.pem
COPY certs/sdp-injector-crt.pem certs/cert.pem
CMD ["/bin/appgate-appgated"]