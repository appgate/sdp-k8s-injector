FROM debian:bullseye-slim
LABEL sdp=true
LABEL project=sdp-k8s-client
LABEL image=sdp-identity-service
WORKDIR /sdp-k8s-client
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates openssl && \
    apt-get autoremove --purge -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY  --from=sdp-k8s-client-build:latest /sdp-build/bin/sdp-identity-service /sdp-k8s-client
COPY sdp-identity-service/log4rs.yaml /sdp-k8s-client/log4rs.yaml
ENTRYPOINT ["/sdp-k8s-client/sdp-identity-service"]
