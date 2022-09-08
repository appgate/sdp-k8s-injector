FROM debian:bullseye-slim
ARG PROFILE
ENV PROFILE=${PROFILE:-debug}
LABEL sdp=true
LABEL project=sdp-k8s-client
LABEL image=sdp-device-id-service
WORKDIR /sdp-k8s-client
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates openssl && \
    apt-get autoremove --purge -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=sdp-k8s-client:latest /sdp-build/target/$PROFILE/sdp-device-id-service /sdp-k8s-client
ENTRYPOINT ["/sdp-k8s-client/sdp-device-id-service"]
