# prequal-router image. Build from the repository root:
#   docker build -f deploy/Dockerfile -t prequal-router .
FROM rust:1.89-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p prequal-router

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /src/target/release/prequal-router /usr/local/bin/prequal-router
EXPOSE 8000 8081
# Runs as PID 1: the router installs its own SIGTERM handler and drains (--drain-secs) before exiting.
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/prequal-router"]
