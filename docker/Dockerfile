FROM rust:1-alpine as sdp-builder
LABEL builder=true
LABEL sdp=true
LABEL project=sdp-injector
WORKDIR /usr/src/sdp-injector
RUN apk add --no-cache musl-dev && apk add --no-cache openssl-dev
RUN cargo init
COPY Cargo.lock .
COPY Cargo.toml .
RUN cargo build --release
RUN rm src/*rs
RUN rm target/release/deps/sdp_injector*
COPY src src
RUN cargo build --release

FROM alpine
LABEL sdp=true
LABEL project=sdp-injector
COPY --from=sdp-builder /usr/src/sdp-injector/target/release/sdp-injector /bin/sdp-injector
WORKDIR /sdp-injector
COPY sidecars.json .
CMD ["/usr/bin/sdp-injector"]