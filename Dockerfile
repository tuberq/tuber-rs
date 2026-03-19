FROM --platform=$BUILDPLATFORM rust:1.94-bookworm AS builder
ARG TARGETARCH

RUN case "$TARGETARCH" in \
      amd64) echo "x86_64-unknown-linux-musl" > /target.txt && apt-get update && apt-get install -y musl-tools ;; \
      arm64) echo "aarch64-unknown-linux-musl" > /target.txt && apt-get update && apt-get install -y musl-tools gcc-aarch64-linux-gnu && \
             echo "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc" >> /etc/environment ;; \
    esac && \
    rustup target add $(cat /target.txt)

WORKDIR /src
COPY . .

RUN export $(cat /etc/environment 2>/dev/null | xargs) && \
    cargo build --release --target $(cat /target.txt) && \
    cp target/$(cat /target.txt)/release/tuber-tui /tuber-tui

FROM scratch
COPY --from=builder /tuber-tui /tuber-tui
ENTRYPOINT ["/tuber-tui"]
