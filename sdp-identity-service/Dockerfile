FROM rust:1.63
WORKDIR /usr/src/appgate
COPY . .
RUN cargo install --path sdp-identity-service
RUN rm -rf ./*
ENTRYPOINT ["sdp-identity-service"]
