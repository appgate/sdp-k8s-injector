FROM rust:1.63
WORKDIR /usr/src/appgate
COPY . .
RUN cargo install --path sdp-device-id-service
RUN rm -rf ./*
ENTRYPOINT ["sdp-device-id-service"]
