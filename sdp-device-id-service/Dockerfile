FROM debian:bullseye-slim
LABEL sdp=true
LABEL project=sdp-k8s-injector
LABEL image=sdp-device-id-service
WORKDIR /sdp-k8s-injector
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates openssl && \
    apt-get autoremove --purge -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=sdp-k8s-injector-build:latest /sdp-build/bin/sdp-device-id-service /sdp-k8s-injector
ENTRYPOINT ["/sdp-k8s-injector/sdp-device-id-service"]
