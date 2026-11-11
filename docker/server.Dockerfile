FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app
# Link glibc statically, so the binary runs in an empty image.
# The explicit --target keeps this flag away from build scripts and proc macros, which
# must stay dynamic. glibc 2.34 and newer has the "files" and "dns" host lookups inside
# libc, so DNS works without the shared NSS libraries.
ENV RUSTFLAGS="-C target-feature=+crt-static"
RUN rustc -vV | sed -n 's/^host: //p' > /rust-target

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# This layer builds only the dependencies, so Docker can cache it between code changes.
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --target "$(cat /rust-target)" --recipe-path recipe.json
COPY . .
RUN cargo build --release --target "$(cat /rust-target)" -p server --bin server \
    && cp "target/$(cat /rust-target)/release/server" /toolport-server \
    && if readelf -lW /toolport-server | grep -q INTERP || readelf -dW /toolport-server | grep -q NEEDED; then \
        echo "the binary is not static" >&2; exit 1; fi
RUN echo "toolport:x:10001:10001::/:/sbin/nologin" > /passwd \
    && echo "toolport:x:10001:" > /group

FROM scratch
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /passwd /etc/passwd
COPY --from=builder /group /etc/group
COPY --from=builder /toolport-server /usr/local/bin/toolport-server
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/toolport-server"]
