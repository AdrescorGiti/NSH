FROM rust:1.85-bookworm AS builder
WORKDIR /opt/nsh
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples ./examples
RUN cargo build --locked --release

FROM python:3.12-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends git gcc make autoconf automake bash ca-certificates python3 && rm -rf /var/lib/apt/lists/*
WORKDIR /opt/nsh
COPY --from=builder /opt/nsh/target/release/nsh target/release/nsh
COPY tests ./tests
COPY examples ./examples
RUN (timeout 30s git clone --depth 1 --branch v0.5.13.5 https://github.com/herbertx/dash.git /tmp/dash || \
    (rm -rf /tmp/dash && timeout 30s git clone --depth 1 --branch v0.5.13.5 https://git.kernel.org/pub/scm/utils/dash/dash.git /tmp/dash)) && \
    test "$(git -C /tmp/dash rev-parse HEAD)" = 037bbdfd330017c368caf6242f977974123239b5 && \
    cd /tmp/dash && ./autogen.sh && ./configure && make -j2 && install src/dash /usr/local/bin/hardtest-dash && \
    git clone --depth 1 --branch 2.61 https://github.com/magicant/yash.git /tmp/yash && \
    test "$(git -C /tmp/yash rev-parse HEAD)" = 519475a68692ba3c375e40a62a39b2b48d4a431f && \
    cd /tmp/yash && ./configure && make -j2 yash && install yash /usr/local/bin/hardtest-yash && \
    rm -rf /tmp/dash /tmp/yash
COPY hardtest ./hardtest
USER 65534:65534
ENTRYPOINT ["python3", "/opt/nsh/hardtest/inside.py"]
