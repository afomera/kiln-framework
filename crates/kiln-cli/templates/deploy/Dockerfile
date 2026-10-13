# syntax=docker/dockerfile:1
# The production image. Build it with `kiln build --docker` (or `docker build -t @@app@@ .`),
# then run:
#
#   docker run -p 8080:8080 -e KILN_SECRET_KEY=<64+ random characters> \
#     -v @@app@@-storage:/app/storage @@app@@
#
# SQLite and uploaded files live in /app/storage (keep it on a volume); set DATABASE_URL and
# STORAGE_URL to use Postgres and S3 instead. Migrations run on boot (KILN_MIGRATE_ON_BOOT=0
# skips them). GET /up is the health check.

FROM rust:1-slim-bookworm AS build
WORKDIR /build
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked && cp target/release/@@app@@ /usr/local/bin/@@app@@

FROM debian:bookworm-slim
RUN apt-get update -qq \
 && apt-get install --no-install-recommends -y ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 1000 app
WORKDIR /app
# One file: templates, assets, migrations and secrets are compiled into it.
COPY --from=build /usr/local/bin/@@app@@ /app/bin/@@app@@
COPY --from=build /build/bin/docker-entrypoint /app/bin/docker-entrypoint
RUN mkdir -p /app/storage && chown app:app /app/storage
USER app
ENV KILN_ENV=production PORT=8080
VOLUME /app/storage
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=5s --start-period=15s CMD ["/app/bin/@@app@@", "health"]
ENTRYPOINT ["/app/bin/docker-entrypoint"]
CMD ["serve"]
