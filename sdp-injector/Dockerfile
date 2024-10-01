FROM debian:bookworm-slim
LABEL sdp=true
LABEL project=sdp-k8s-injector
LABEL image=sdp-injector
WORKDIR /sdp-k8s-injector
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates openssl && \
    apt-get autoremove --purge -y && \
    rm -rf /var/lib/apt/lists/*
COPY --from=sdp-k8s-injector-build:latest /sdp-build/bin/sdp-injector /sdp-k8s-injector
ENTRYPOINT ["/sdp-k8s-injector/sdp-injector"]
