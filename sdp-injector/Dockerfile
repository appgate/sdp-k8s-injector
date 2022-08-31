FROM rust:1-alpine as sdp-builder
ARG PROFILE
ENV PROFILE=${PROFILE:-debug}
ENV DOCKER_REGISTRY=ghcr.io/appgate/sdp-k8s-client
LABEL builder=true
LABEL sdp=true
LABEL project=sdp-injector
WORKDIR /usr/src/sdp-injector
RUN apk add --no-cache musl-dev && apk add --no-cache openssl-dev && apk add --no-cache gettext
COPY . .
COPY tests/sdp-sidecars.json tests/sdp-sidecars.json
RUN cargo test $([ "$PROFILE" = "release" ] && echo --release) --all-targets --bin sdp-injector
RUN cargo build $([ "$PROFILE" = "release" ] && echo --release) --bin sdp-injector

FROM alpine:3.14.0
ARG PROFILE
ENV PROFILE=${PROFILE:-debug}
LABEL sdp=true
LABEL project=sdp-injector
COPY --from=sdp-builder /usr/src/sdp-injector/target/$PROFILE/sdp-injector /usr/bin/sdp-injector
WORKDIR /sdp-injector
CMD ["/usr/bin/sdp-injector"]
