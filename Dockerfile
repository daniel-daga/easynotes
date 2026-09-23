# Build from the repository root so Cargo can resolve the workspace and the
# relay's path dependency on easynotes-core.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p easynotes-relay

FROM debian:bookworm-slim AS runtime
RUN useradd --system --uid 10001 --no-create-home relay
COPY --from=build /src/target/release/easynotes-relay /usr/local/bin/easynotes-relay
USER 10001:10001
EXPOSE 8080
ENV RELAY_ADDR=0.0.0.0:8080
ENTRYPOINT ["/usr/local/bin/easynotes-relay"]
